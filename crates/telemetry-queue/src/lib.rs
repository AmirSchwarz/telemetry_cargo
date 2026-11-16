//! Bounded, oldest-first queues. Batches remain queued until acknowledged.
//!
//! Byte limits refer to serialized event payloads, not SQLite file allocation.
//! SQLite is plaintext; callers must provision filesystem permissions and keys
//! separately if encrypted persistence is required.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, VecDeque},
    fmt,
};
use telemetry_schema::{validate_envelope, Category, Event, Limits};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueConfig {
    pub max_events: usize,
    pub max_bytes: usize,
    pub ttl_ms: u64,
    pub per_category_max_events: usize,
}
impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            max_events: 1024,
            max_bytes: 4 * 1024 * 1024,
            ttl_ms: 86_400_000,
            per_category_max_events: 512,
        }
    }
}
impl QueueConfig {
    pub fn validate(&self) -> Result<(), QueueError> {
        if self.max_events == 0
            || self.max_events > 100_000
            || self.max_bytes == 0
            || self.max_bytes > 128 * 1024 * 1024
            || self.ttl_ms == 0
            || self.ttl_ms > 30 * 86_400_000
            || self.per_category_max_events == 0
            || self.per_category_max_events > self.max_events
        {
            return Err(QueueError("invalid queue budgets"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QueueError(pub &'static str);
impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for QueueError {}

pub trait EventQueue: Send {
    /// Returns the number of expired or evicted entries. Oversized events fail
    /// without disturbing the existing queue. Duplicate event IDs are no-ops.
    fn enqueue(&mut self, event: Event, now_ms: u64) -> Result<usize, QueueError>;
    fn batch(&mut self, limit: usize, now_ms: u64) -> Result<Vec<Event>, QueueError>;
    fn acknowledge(&mut self, ids: &[String]) -> Result<(), QueueError>;
    fn retain_categories(&mut self, allowed: &BTreeSet<Category>) -> Result<(), QueueError>;
    fn len(&self) -> usize;
    fn byte_len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone)]
struct Entry {
    event: Event,
    encoded: String,
    queued_at: u64,
}

#[derive(Clone)]
pub struct MemoryQueue {
    config: QueueConfig,
    entries: VecDeque<Entry>,
    bytes: usize,
}
impl MemoryQueue {
    pub fn new(config: QueueConfig) -> Result<Self, QueueError> {
        config.validate()?;
        Ok(Self {
            config,
            entries: VecDeque::new(),
            bytes: 0,
        })
    }

    fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.entries.len();
        // Future timestamps can occur after a wall-clock correction. A record
        // cannot outlive TTL by more than that correction; on restart suspicious
        // future queue timestamps are rejected instead.
        self.entries
            .retain(|entry| now_ms.saturating_sub(entry.queued_at) < self.config.ttl_ms);
        self.recount();
        before - self.entries.len()
    }

    fn recount(&mut self) {
        self.bytes = self.entries.iter().map(|e| e.encoded.len()).sum();
    }

    fn enforce_bounds(&mut self) {
        for category in Category::ALL {
            while self
                .entries
                .iter()
                .filter(|e| e.event.category == category)
                .count()
                > self.config.per_category_max_events
            {
                if let Some(index) = self
                    .entries
                    .iter()
                    .position(|e| e.event.category == category)
                {
                    if let Some(entry) = self.entries.remove(index) {
                        self.bytes -= entry.encoded.len();
                    }
                }
            }
        }
        while self.entries.len() > self.config.max_events || self.bytes > self.config.max_bytes {
            if let Some(entry) = self.entries.pop_front() {
                self.bytes -= entry.encoded.len();
            } else {
                break;
            }
        }
    }
}

impl EventQueue for MemoryQueue {
    fn enqueue(&mut self, event: Event, now_ms: u64) -> Result<usize, QueueError> {
        // The engine validates the exact registry and configured event limits.
        // Here an independent maximum protects standalone queue consumers.
        validate_envelope(
            &event,
            &Limits {
                max_properties: 128,
                max_string_bytes: 4096,
                max_event_bytes: 1024 * 1024,
            },
        )
        .map_err(|_| QueueError("invalid event envelope"))?;
        let encoded =
            serde_json::to_string(&event).map_err(|_| QueueError("event encoding failed"))?;
        if encoded.len() > self.config.max_bytes {
            return Err(QueueError("event exceeds queue byte budget"));
        }
        let expired = self.expire(now_ms);
        if self
            .entries
            .iter()
            .any(|e| e.event.event_id == event.event_id)
        {
            return Ok(expired);
        }
        let before = self.entries.len();
        self.bytes += encoded.len();
        self.entries.push_back(Entry {
            event,
            encoded,
            queued_at: now_ms,
        });
        self.enforce_bounds();
        Ok(expired + before + 1 - self.entries.len())
    }

    fn batch(&mut self, limit: usize, now_ms: u64) -> Result<Vec<Event>, QueueError> {
        self.expire(now_ms);
        Ok(self
            .entries
            .iter()
            .take(limit)
            .map(|e| e.event.clone())
            .collect())
    }

    fn acknowledge(&mut self, ids: &[String]) -> Result<(), QueueError> {
        let ids: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
        self.entries
            .retain(|e| !ids.contains(e.event.event_id.as_str()));
        self.recount();
        Ok(())
    }

    fn retain_categories(&mut self, allowed: &BTreeSet<Category>) -> Result<(), QueueError> {
        self.entries.retain(|e| allowed.contains(&e.event.category));
        self.recount();
        Ok(())
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
    fn byte_len(&self) -> usize {
        self.bytes
    }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;
    use rusqlite::{params, Connection, OpenFlags};
    use std::{path::Path, time::Duration};
    use telemetry_schema::{now_unix_ms, Registry};

    /// Single-owner SQLite WAL queue with transactional mutations. It retains an
    /// in-memory index; disk operations finish before the logical index changes.
    /// Stored records for unknown/custom definitions are discarded on reopening
    /// because a fresh engine has not registered those definitions yet. A file
    /// has one live queue owner; a second owner fails to acquire its SQLite lock.
    /// On Unix, files are created with mode 0600; existing nonprivate files and
    /// symbolic links are rejected. Windows deployments must provision ACLs.
    pub struct SqliteQueue {
        connection: Connection,
        memory: MemoryQueue,
    }

    impl SqliteQueue {
        pub fn open(path: impl AsRef<Path>, config: QueueConfig) -> Result<Self, QueueError> {
            config.validate()?;
            let path = path.as_ref();
            prepare_path(path)?;
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|_| QueueError("cannot open queue database"))?;
            connection
                .busy_timeout(Duration::from_millis(100))
                .map_err(|_| QueueError("cannot configure queue database"))?;
            connection
                .execute_batch(
                    "PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
                 PRAGMA journal_size_limit=1048576;
                 CREATE TABLE IF NOT EXISTS telemetry_queue_v1 (
                    event_id TEXT PRIMARY KEY NOT NULL,
                    queued_at INTEGER NOT NULL,
                    payload TEXT NOT NULL
                 );",
                )
                .map_err(|_| QueueError("cannot initialize queue database"))?;
            let mut memory = MemoryQueue::new(config)?;
            let now = now_unix_ms();
            let registry = Registry::default();
            {
                let mut statement = connection
                    .prepare("SELECT queued_at, payload FROM telemetry_queue_v1 WHERE length(payload)<=1048576 ORDER BY rowid")
                    .map_err(|_| QueueError("cannot inspect queue database"))?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(|_| QueueError("cannot read queue database"))?;
                // Bound each record before parsing; the configured payload
                // budget is also reapplied to the resulting memory index.
                for row in rows {
                    let (queued_at, encoded) =
                        row.map_err(|_| QueueError("corrupt queue database"))?;
                    if queued_at < 0
                        || queued_at as u64 > now
                        || now.saturating_sub(queued_at as u64) >= memory.config.ttl_ms
                        || encoded.len() > 1024 * 1024
                    {
                        continue;
                    }
                    let Ok(event) = serde_json::from_str::<Event>(&encoded) else {
                        continue;
                    };
                    if registry.validate(&event, &Limits::default()).is_err() {
                        continue;
                    }
                    if encoded.len() > memory.config.max_bytes {
                        continue;
                    }
                    memory.bytes += encoded.len();
                    memory.entries.push_back(Entry {
                        event,
                        encoded,
                        queued_at: queued_at as u64,
                    });
                    memory.enforce_bounds();
                }
            }
            let mut queue = Self {
                connection,
                memory: MemoryQueue::new(memory.config.clone())?,
            };
            // Rewrite startup data atomically, deleting corrupt, expired,
            // incompatible and over-budget records before exposing any batch.
            queue.replace_all(memory)?;
            queue
                .connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(|_| QueueError("cannot checkpoint queue database"))?;
            Ok(queue)
        }

        fn replace_all(&mut self, candidate: MemoryQueue) -> Result<(), QueueError> {
            let transaction = self
                .connection
                .transaction()
                .map_err(|_| QueueError("queue transaction failed"))?;
            transaction
                .execute("DELETE FROM telemetry_queue_v1", [])
                .map_err(|_| QueueError("queue purge failed"))?;
            {
                let mut insert = transaction.prepare(
                    "INSERT INTO telemetry_queue_v1(event_id,queued_at,payload) VALUES(?1,?2,?3)"
                ).map_err(|_| QueueError("queue transaction failed"))?;
                for entry in &candidate.entries {
                    insert
                        .execute(params![
                            &entry.event.event_id,
                            i64::try_from(entry.queued_at)
                                .map_err(|_| QueueError("queue timestamp overflow"))?,
                            &entry.encoded
                        ])
                        .map_err(|_| QueueError("queue write failed"))?;
                }
            }
            transaction
                .commit()
                .map_err(|_| QueueError("queue commit failed"))?;
            self.memory = candidate;
            Ok(())
        }

        fn persist(&mut self, candidate: MemoryQueue) -> Result<(), QueueError> {
            let transaction = self
                .connection
                .transaction()
                .map_err(|_| QueueError("queue transaction failed"))?;
            {
                let retained: BTreeSet<&str> = candidate
                    .entries
                    .iter()
                    .map(|e| e.event.event_id.as_str())
                    .collect();
                let previous: BTreeSet<&str> = self
                    .memory
                    .entries
                    .iter()
                    .map(|e| e.event.event_id.as_str())
                    .collect();
                for id in previous.difference(&retained) {
                    transaction
                        .execute("DELETE FROM telemetry_queue_v1 WHERE event_id=?1", [id])
                        .map_err(|_| QueueError("queue delete failed"))?;
                }
                for entry in &candidate.entries {
                    if !previous.contains(entry.event.event_id.as_str()) {
                        transaction.execute(
                            "INSERT INTO telemetry_queue_v1(event_id,queued_at,payload) VALUES(?1,?2,?3)",
                            params![&entry.event.event_id,
                                i64::try_from(entry.queued_at).map_err(|_| QueueError("queue timestamp overflow"))?,
                                &entry.encoded]
                        ).map_err(|_| QueueError("queue write failed"))?;
                    }
                }
            }
            transaction
                .commit()
                .map_err(|_| QueueError("queue commit failed"))?;
            self.memory = candidate;
            Ok(())
        }
    }

    fn prepare_path(path: &Path) -> Result<(), QueueError> {
        if path.as_os_str().is_empty() {
            return Err(QueueError("queue path cannot be empty"));
        }
        if path == Path::new(":memory:") {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(QueueError("cannot create private queue database")),
            }
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|_| QueueError("cannot inspect queue database permissions"))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(QueueError(
                    "queue database requires private file permissions",
                ));
            }
        }
        Ok(())
    }

    impl EventQueue for SqliteQueue {
        fn enqueue(&mut self, event: Event, now_ms: u64) -> Result<usize, QueueError> {
            let mut candidate = self.memory.clone();
            let dropped = candidate.enqueue(event, now_ms)?;
            self.persist(candidate)?;
            Ok(dropped)
        }
        fn batch(&mut self, limit: usize, now_ms: u64) -> Result<Vec<Event>, QueueError> {
            let mut candidate = self.memory.clone();
            let batch = candidate.batch(limit, now_ms)?;
            if candidate.len() != self.memory.len() {
                self.persist(candidate)?;
            }
            Ok(batch)
        }
        fn acknowledge(&mut self, ids: &[String]) -> Result<(), QueueError> {
            let mut candidate = self.memory.clone();
            candidate.acknowledge(ids)?;
            self.persist(candidate)
        }
        fn retain_categories(&mut self, allowed: &BTreeSet<Category>) -> Result<(), QueueError> {
            let mut candidate = self.memory.clone();
            candidate.retain_categories(allowed)?;
            self.persist(candidate)?;
            // Purged values are made inaccessible in the main DB by secure_delete;
            // truncate WAL too. This is not a forensic erasure guarantee on SSDs.
            self.connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(|_| QueueError("cannot checkpoint purged queue"))?;
            Ok(())
        }
        fn len(&self) -> usize {
            self.memory.len()
        }
        fn byte_len(&self) -> usize {
            self.memory.byte_len()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn failed_disk_mutations_preserve_index_and_unacknowledged_events() {
            let mut queue = SqliteQueue::open(":memory:", QueueConfig::default()).unwrap();
            let first: Event =
                serde_json::from_str(include_str!("../schema/action-v1.json")).unwrap();
            queue.enqueue(first.clone(), now_unix_ms()).unwrap();
            queue
                .connection
                .execute_batch("PRAGMA query_only=ON;")
                .unwrap();
            assert!(queue
                .acknowledge(std::slice::from_ref(&first.event_id))
                .is_err());
            let mut second = first.clone();
            second.event_id = telemetry_schema::new_id();
            assert!(queue.enqueue(second, now_unix_ms()).is_err());
            assert_eq!(queue.batch(100, now_unix_ms()).unwrap(), vec![first]);
        }

        #[test]
        fn a_second_owner_cannot_restore_a_stale_queue() {
            let path = std::env::temp_dir().join(format!(
                "telemetry-owner-{}.sqlite",
                telemetry_schema::new_id()
            ));
            let queue = SqliteQueue::open(&path, QueueConfig::default()).unwrap();
            assert!(SqliteQueue::open(&path, QueueConfig::default()).is_err());
            drop(queue);
            assert!(SqliteQueue::open(&path, QueueConfig::default()).is_ok());
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteQueue;

#[cfg(test)]
mod tests {
    use super::*;
    use telemetry_schema::{new_id, Properties};

    fn event(category: Category, id: &str) -> Event {
        let (name, properties) = if category == Category::Diagnostics {
            (
                "error",
                Properties::from([
                    ("code".into(), id.into()),
                    ("message".into(), "[redacted]".into()),
                ]),
            )
        } else {
            (
                "action",
                Properties::from([
                    ("id".into(), id.into()),
                    ("result".into(), "success".into()),
                ]),
            )
        };
        Event {
            schema_version: 1,
            event_id: new_id(),
            app_id: "test".into(),
            app_version: "1.0".into(),
            platform: "linux".into(),
            sdk_version: "0.1.0".into(),
            category,
            event_name: name.into(),
            occurred_at: 1,
            properties,
            session_id: None,
            workflow_id: None,
        }
    }
    fn config() -> QueueConfig {
        QueueConfig {
            max_events: 3,
            max_bytes: 4096,
            per_category_max_events: 2,
            ttl_ms: 100,
        }
    }

    #[test]
    fn quotas_evict_oldest_of_category_and_ack_is_explicit() {
        let mut queue = MemoryQueue::new(config()).unwrap();
        let a = event(Category::UsageAggregate, "a");
        let b = event(Category::Diagnostics, "b");
        let c = event(Category::UsageAggregate, "c");
        let d = event(Category::UsageAggregate, "d");
        queue.enqueue(a.clone(), 0).unwrap();
        queue.enqueue(b.clone(), 0).unwrap();
        queue.enqueue(c.clone(), 0).unwrap();
        assert_eq!(queue.enqueue(d.clone(), 0).unwrap(), 1);
        assert_eq!(queue.batch(10, 0).unwrap(), vec![b.clone(), c, d]);
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.batch(1, 0).unwrap(), vec![b.clone()]);
        assert_eq!(queue.len(), 3);
        queue.acknowledge(&[b.event_id]).unwrap();
        assert_eq!(queue.len(), 2);
    }
    #[test]
    fn ttl_and_consent_purge() {
        let mut queue = MemoryQueue::new(config()).unwrap();
        queue
            .enqueue(event(Category::UsageAggregate, "a"), 10)
            .unwrap();
        queue
            .enqueue(event(Category::Diagnostics, "b"), 30)
            .unwrap();
        assert_eq!(queue.batch(10, 110).unwrap().len(), 1);
        queue.retain_categories(&BTreeSet::new()).unwrap();
        assert!(queue.is_empty());
        assert_eq!(queue.byte_len(), 0);
    }
    #[test]
    fn bytes_are_bounded_and_failed_insertion_preserves_existing() {
        let a = event(Category::UsageAggregate, "a");
        let bytes = serde_json::to_vec(&a).unwrap().len();
        let mut queue = MemoryQueue::new(QueueConfig {
            max_bytes: bytes,
            ..config()
        })
        .unwrap();
        queue.enqueue(a.clone(), 0).unwrap();
        let mut large = event(Category::UsageAggregate, "larger");
        large.app_version = "1.0.0-more".into();
        assert!(queue.enqueue(large, 0).is_err());
        assert_eq!(queue.batch(10, 0).unwrap(), vec![a]);
        assert!(queue.byte_len() <= bytes);
    }
    #[test]
    fn duplicate_send_ids_do_not_duplicate_queue() {
        let mut queue = MemoryQueue::new(config()).unwrap();
        let a = event(Category::UsageAggregate, "a");
        queue.enqueue(a.clone(), 0).unwrap();
        queue.enqueue(a, 1).unwrap();
        assert_eq!(queue.len(), 1);
    }
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_reopens_and_purges_expired_invalid_and_withdrawn_data() {
        let path = std::env::temp_dir().join(format!("telemetry-queue-{}.sqlite", new_id()));
        let now = telemetry_schema::now_unix_ms();
        let cfg = QueueConfig {
            ttl_ms: 60_000,
            ..config()
        };
        let a = event(Category::UsageAggregate, "a");
        {
            let mut queue = SqliteQueue::open(&path, cfg.clone()).unwrap();
            queue.enqueue(a.clone(), now).unwrap();
            queue
                .enqueue(event(Category::Diagnostics, "b"), now - 60_000)
                .unwrap();
        }
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute(
                "INSERT INTO telemetry_queue_v1 VALUES(?1,?2,?3)",
                rusqlite::params!["invalid", now as i64, "{broken"],
            )
            .unwrap();
        }
        {
            let mut queue = SqliteQueue::open(&path, cfg.clone()).unwrap();
            assert_eq!(queue.batch(10, now).unwrap(), vec![a]);
            queue.retain_categories(&BTreeSet::new()).unwrap();
        }
        let queue = SqliteQueue::open(&path, cfg).unwrap();
        assert!(queue.is_empty());
        drop(queue);
        let _ = std::fs::remove_file(path);
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_restore_purges_diagnostics_with_session_linkage() {
        let path = std::env::temp_dir().join(format!("telemetry-linked-{}.sqlite", new_id()));
        let now = telemetry_schema::now_unix_ms();
        let config = QueueConfig {
            ttl_ms: 60_000,
            ..config()
        };
        drop(SqliteQueue::open(&path, config.clone()).unwrap());
        let mut diagnostic = event(Category::Diagnostics, "io.failure");
        diagnostic.session_id = Some(new_id());
        let mut workflow_linked = event(Category::Diagnostics, "io.failure");
        workflow_linked.workflow_id = Some(new_id());
        // Simulate records written by an earlier SDK, bypassing current enqueue
        // validation. Valid diagnostics remain; linked records must be removed.
        let valid = event(Category::Diagnostics, "io.failure");
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            for record in [&diagnostic, &workflow_linked, &valid] {
                db.execute(
                    "INSERT INTO telemetry_queue_v1 VALUES(?1,?2,?3)",
                    rusqlite::params![
                        record.event_id,
                        now as i64,
                        serde_json::to_string(record).unwrap()
                    ],
                )
                .unwrap();
            }
        }
        {
            let mut queue = SqliteQueue::open(&path, config).unwrap();
            assert_eq!(queue.batch(10, now).unwrap(), vec![valid]);
        }
        let db = rusqlite::Connection::open(&path).unwrap();
        let retained: i64 = db
            .query_row("SELECT count(*) FROM telemetry_queue_v1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained, 1);
        drop(db);
        let _ = std::fs::remove_file(path);
    }
    #[cfg(feature = "sqlite")]
    #[test]
    fn corrupt_database_fails_closed() {
        let path = std::env::temp_dir().join(format!("telemetry-corrupt-{}.sqlite", new_id()));
        std::fs::write(&path, b"This is not a SQLite database").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(SqliteQueue::open(&path, config()).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[cfg(all(feature = "sqlite", unix))]
    #[test]
    fn sqlite_rejects_nonprivate_files_and_symlink_paths() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let path = std::env::temp_dir().join(format!("telemetry-perms-{}.sqlite", new_id()));
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SqliteQueue::open(&path, config()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = path.with_extension("link");
        symlink(&path, &alias).unwrap();
        assert!(SqliteQueue::open(&alias, config()).is_err());
        let _ = std::fs::remove_file(alias);
        let _ = std::fs::remove_file(path);
    }
}
