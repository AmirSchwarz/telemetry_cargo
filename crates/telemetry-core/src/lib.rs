//! Shared privacy-enforcing telemetry engine. Network I/O runs on a worker.
mod aggregation;
mod command;
mod config;
pub mod lifecycle;
#[cfg(test)]
mod tests;
mod worker;

pub use aggregation::{Aggregates, CounterAggregate, SpanAggregate, SPAN_BUCKET_BOUNDS_MS};
pub use config::Config;
pub use telemetry_privacy::Consent;
pub use telemetry_queue::QueueConfig;
pub use telemetry_schema::{Category, Event, EventDefinition, Limits, Properties, PropertyRule};
pub use telemetry_transport::{Transport, TransportError};

use lifecycle::Lifecycle;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use telemetry_privacy::PrivacyGate;
use telemetry_queue::{EventQueue, MemoryQueue};
use telemetry_schema::{new_id, now_unix_ms, validate_id, Registry};

const MAX_STATE_ENTRIES: usize = 256;
const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig(&'static str),
    InvalidInput(&'static str),
    UnknownEvent,
    Queue,
    Closed,
    DeadlineExceeded,
    Transport { retryable: bool },
    NoTransport,
    Capacity,
    Poisoned,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) | Self::InvalidInput(reason) => f.write_str(reason),
            Self::UnknownEvent => f.write_str("event is not registered"),
            Self::Queue => f.write_str("telemetry queue operation failed"),
            Self::Closed => f.write_str("telemetry is shut down"),
            Self::DeadlineExceeded => {
                f.write_str("telemetry deadline exceeded; pending events retained")
            }
            Self::Transport { .. } => {
                f.write_str("telemetry export failed; pending events retained")
            }
            Self::NoTransport => f.write_str("no exporter configured; pending events retained"),
            Self::Capacity => f.write_str("telemetry state limit reached"),
            Self::Poisoned => f.write_str("telemetry state is unavailable"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionResult {
    Success,
    Failure,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOutcome {
    Complete,
    Cancelled,
    Failed,
    Timeout,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Diagnostics {
    pub queue_depth: usize,
    pub queued_bytes: usize,
    pub accepted: u64,
    pub dropped: u64,
    pub denied: u64,
    pub exporter_failures: u64,
    pub closed: bool,
    pub consent: BTreeMap<Category, bool>,
    pub consent_epoch: u64,
}

pub struct Telemetry {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    /// Serializes consent withdrawal and sends without blocking normal collection.
    upload_gate: Mutex<()>,
    transport: Option<Arc<dyn Transport>>,
    handles: AtomicUsize,
}

struct State {
    config: Config,
    gate: PrivacyGate,
    registry: Registry,
    queue: Box<dyn EventQueue>,
    session_id: Option<String>,
    consent_epoch: u64,
    lifecycle: Option<Lifecycle>,
    screens: BTreeMap<String, u64>,
    workflows: BTreeMap<String, Instant>,
    origin: Instant,
    diagnostics: Diagnostics,
    aggregates: Aggregates,
    closing: bool,
    stopped: bool,
    requested: bool,
    flush_deadline: Option<Instant>,
    next_export: Instant,
    retry_at: Instant,
    failures: u32,
    send_attempt: u64,
    last_send_error: Option<bool>,
}

impl Clone for Telemetry {
    fn clone(&self) -> Self {
        self.inner.handles.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Telemetry")
            .field("diagnostics", &self.diagnostics())
            .finish_non_exhaustive()
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        if self.inner.handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            if let Ok(mut state) = self.inner.state.lock() {
                state.stopped = true;
                state.closing = true;
            }
            self.inner.changed.notify_all();
        }
    }
}

impl State {
    fn open(&self) -> Result<(), Error> {
        if self.closing || self.stopped {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    fn tick(&self) -> u64 {
        self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }

    fn eligible(&mut self, category: Category) -> Result<bool, Error> {
        self.open()?;
        if !self.gate.enabled(category) {
            self.diagnostics.denied = self.diagnostics.denied.saturating_add(1);
            return Ok(false);
        }
        Ok(true)
    }

    fn emit(
        &mut self,
        name: &str,
        mut properties: Properties,
        workflow_id: Option<String>,
    ) -> Result<(), Error> {
        let definition = self.registry.definition(name).ok_or(Error::UnknownEvent)?;
        if !self.gate.enabled(definition.category) {
            self.diagnostics.denied = self.diagnostics.denied.saturating_add(1);
            return Ok(());
        }
        self.gate.sanitize(definition, &mut properties);
        let category = definition.category;
        let event = Event {
            schema_version: 1,
            event_id: new_id(),
            app_id: self.config.app_id.clone(),
            app_version: self.config.app_version.clone(),
            platform: std::env::consts::OS.to_owned(),
            sdk_version: SDK_VERSION.to_owned(),
            category,
            event_name: name.to_owned(),
            occurred_at: now_unix_ms(),
            properties,
            session_id: if category == Category::UsageSession {
                self.session_id.clone()
            } else {
                None
            },
            workflow_id,
        };
        self.registry
            .validate(&event, &self.config.limits)
            .map_err(|_| Error::InvalidInput("event violates registry or data limits"))?;
        let dropped = self
            .queue
            .enqueue(event, now_unix_ms())
            .map_err(|_| Error::Queue)?;
        self.diagnostics.accepted = self.diagnostics.accepted.saturating_add(1);
        self.diagnostics.dropped = self.diagnostics.dropped.saturating_add(dropped as u64);
        Ok(())
    }

    fn running_time(&mut self) -> u64 {
        let tick = self.tick();
        self.lifecycle.as_mut().map_or(0, |lifecycle| {
            lifecycle.advance(tick);
            lifecycle.totals().running_ms
        })
    }

    fn diagnostic_snapshot(&self) -> Diagnostics {
        Diagnostics {
            queue_depth: self.queue.len(),
            queued_bytes: self.queue.byte_len(),
            closed: self.closing || self.stopped,
            consent: Category::ALL
                .into_iter()
                .map(|category| (category, self.gate.enabled(category)))
                .collect(),
            consent_epoch: self.consent_epoch,
            ..self.diagnostics.clone()
        }
    }

    fn record_span(
        &mut self,
        name: &str,
        duration_ms: f64,
        epoch: Option<u64>,
    ) -> Result<(), Error> {
        if !self.eligible(Category::Performance)?
            || epoch.is_some_and(|epoch| epoch != self.consent_epoch)
        {
            return Ok(());
        }
        if !duration_ms.is_finite() || duration_ms < 0.0 {
            return Err(Error::InvalidInput(
                "span duration must be finite and nonnegative",
            ));
        }
        if let Some(existing) = self.aggregates.spans.get(name) {
            if !(existing.sum_ms + duration_ms).is_finite() {
                return Err(Error::InvalidInput("span aggregate exceeds numeric budget"));
            }
        } else if self.aggregates.spans.len() >= MAX_STATE_ENTRIES {
            return Err(Error::Capacity);
        }
        self.emit(
            "span",
            props([("name", json!(name)), ("duration_ms", json!(duration_ms))]),
            None,
        )?;
        self.aggregates
            .spans
            .entry(name.to_owned())
            .and_modify(|aggregate| aggregate.add(duration_ms))
            .or_insert_with(|| SpanAggregate::new(duration_ms));
        Ok(())
    }
}

fn props(values: impl IntoIterator<Item = (&'static str, Value)>) -> Properties {
    values
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

impl Telemetry {
    pub fn init(config: Config) -> Result<Self, Error> {
        let transport: Option<Arc<dyn Transport>> = if let Some(endpoint) = &config.endpoint {
            #[cfg(feature = "http")]
            {
                Some(Arc::new(
                    telemetry_transport::HttpTransport::new(
                        endpoint,
                        config.ingestion_token.clone(),
                    )
                    .map_err(|_| Error::InvalidConfig("invalid HTTPS exporter configuration"))?,
                ))
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = endpoint;
                return Err(Error::InvalidConfig("HTTP exporter feature is not enabled"));
            }
        } else {
            None
        };
        Self::build(config, transport)
    }

    pub fn with_transport(config: Config, transport: Arc<dyn Transport>) -> Result<Self, Error> {
        Self::build(config, Some(transport))
    }

    fn build(mut config: Config, transport: Option<Arc<dyn Transport>>) -> Result<Self, Error> {
        validate_id(&config.app_id)
            .map_err(|_| Error::InvalidConfig("app_id must be a semantic identifier"))?;
        if config.app_version.is_empty() || config.app_version.len() > 128 {
            return Err(Error::InvalidConfig(
                "app_version length must be 1..128 bytes",
            ));
        }
        config
            .queue
            .validate()
            .map_err(|_| Error::InvalidConfig("invalid queue limits"))?;
        config
            .limits
            .validate()
            .map_err(|_| Error::InvalidConfig("invalid event limits"))?;
        if config.batch_size == 0 || config.batch_size > 1_024 {
            return Err(Error::InvalidConfig("batch_size must be 1..1024"));
        }
        if !(1..=86_400_000).contains(&config.idle_threshold_ms) {
            return Err(Error::InvalidConfig(
                "idle_threshold_ms must be 1..86400000",
            ));
        }
        if !(10..=3_600_000).contains(&config.flush_interval_ms) {
            return Err(Error::InvalidConfig(
                "flush_interval_ms must be 10..3600000",
            ));
        }
        let mut queue: Box<dyn EventQueue> = if let Some(path) = &config.sqlite_path {
            #[cfg(feature = "sqlite")]
            {
                Box::new(
                    telemetry_queue::SqliteQueue::open(path, config.queue.clone())
                        .map_err(|_| Error::Queue)?,
                )
            }
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = path;
                return Err(Error::InvalidConfig("SQLite feature is not enabled"));
            }
        } else {
            Box::new(MemoryQueue::new(config.queue.clone()).map_err(|_| Error::Queue)?)
        };
        let gate = PrivacyGate::new(config.consent.clone(), config.essential_operations_reviewed);
        let registry = Registry::default();
        queue
            .retain_categories(&gate.allowed_categories())
            .map_err(|_| Error::Queue)?;
        let invalid: Vec<String> = queue
            .batch(usize::MAX, now_unix_ms())
            .map_err(|_| Error::Queue)?
            .into_iter()
            .filter(|event| {
                event.app_id != config.app_id || registry.validate(event, &config.limits).is_err()
            })
            .map(|event| event.event_id)
            .collect();
        queue.acknowledge(&invalid).map_err(|_| Error::Queue)?;
        let session_enabled = gate.enabled(Category::UsageSession);
        // Tokens and local filesystem locations are needed only during initialization.
        config.ingestion_token = None;
        config.sqlite_path = None;
        let now = Instant::now();
        let interval = Duration::from_millis(config.flush_interval_ms);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                lifecycle: session_enabled.then(|| Lifecycle::new(0, config.idle_threshold_ms)),
                session_id: session_enabled.then(new_id),
                config,
                gate,
                registry,
                queue,
                consent_epoch: 0,
                screens: BTreeMap::new(),
                workflows: BTreeMap::new(),
                origin: now,
                diagnostics: Diagnostics {
                    dropped: invalid.len() as u64,
                    ..Diagnostics::default()
                },
                aggregates: Aggregates::default(),
                closing: false,
                stopped: false,
                requested: false,
                flush_deadline: None,
                next_export: now + interval,
                retry_at: now,
                failures: 0,
                send_attempt: 0,
                last_send_error: None,
            }),
            changed: Condvar::new(),
            upload_gate: Mutex::new(()),
            transport,
            handles: AtomicUsize::new(1),
        });
        if inner.transport.is_some() {
            worker::start(&inner)?;
        }
        Ok(Self { inner })
    }

    fn state(&self) -> Result<MutexGuard<'_, State>, Error> {
        self.inner.state.lock().map_err(|_| Error::Poisoned)
    }

    pub fn set_consent(&self, category: Category, enabled: bool) -> Result<(), Error> {
        let _upload = self.inner.upload_gate.lock().map_err(|_| Error::Poisoned)?;
        let mut state = self.state()?;
        state.open()?;
        let was_enabled = state.gate.enabled(category);
        state.gate.set_consent(category, enabled);
        let is_enabled = state.gate.enabled(category);
        if was_enabled != is_enabled {
            state.consent_epoch = state.consent_epoch.wrapping_add(1);
        }
        if !is_enabled {
            match category {
                Category::UsageAggregate => state.aggregates.counters.clear(),
                Category::Performance => state.aggregates.spans.clear(),
                _ => {}
            }
        }
        if category == Category::UsageSession && was_enabled != is_enabled {
            state.session_id = is_enabled.then(new_id);
            state.screens.clear();
            state.workflows.clear();
            state.lifecycle = if is_enabled {
                Some(Lifecycle::new(state.tick(), state.config.idle_threshold_ms))
            } else {
                None
            };
        }
        let allowed = state.gate.allowed_categories();
        let before = state.queue.len();
        if state.queue.retain_categories(&allowed).is_err() {
            // A failed purge must never leave an exporter able to transmit old records.
            state.stopped = true;
            state.closing = true;
            self.inner.changed.notify_all();
            return Err(Error::Queue);
        }
        state.diagnostics.dropped = state
            .diagnostics
            .dropped
            .saturating_add(before.saturating_sub(state.queue.len()) as u64);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn register_event(&self, definition: EventDefinition) -> Result<(), Error> {
        let mut state = self.state()?;
        state.open()?;
        state
            .registry
            .register(definition)
            .map_err(|_| Error::InvalidInput("invalid or duplicate event definition"))
    }

    pub fn event(&self, name: &str, properties: Properties) -> Result<(), Error> {
        let mut state = self.state()?;
        state.open()?;
        state.emit(name, properties, None)
    }

    fn builtin(
        &self,
        category: Category,
        name: &str,
        properties: impl FnOnce() -> Properties,
    ) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(category)? {
            return Ok(());
        }
        state.emit(name, properties(), None)
    }

    pub fn action(&self, id: &str, result: ActionResult) -> Result<(), Error> {
        self.builtin(Category::UsageAggregate, "action", || {
            props([("id", json!(id)), ("result", json!(result))])
        })
    }

    pub fn capture_error(&self, code: &str, message: &str) -> Result<(), Error> {
        // Never copy raw exception content, even temporarily, into the event model.
        let _ = message;
        self.builtin(Category::Diagnostics, "error", || {
            props([("code", json!(code)), ("message", json!("[redacted]"))])
        })
    }

    /// Rejects a nonfinite amount or sum before enqueueing or changing the aggregate.
    pub fn increment(&self, name: &str, amount: f64) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageAggregate)? {
            return Ok(());
        }
        if !amount.is_finite() {
            return Err(Error::InvalidInput("metric amount must be finite"));
        }
        if let Some(existing) = state.aggregates.counters.get(name) {
            if !(existing.sum + amount).is_finite() {
                return Err(Error::InvalidInput(
                    "counter aggregate exceeds numeric budget",
                ));
            }
        } else if state.aggregates.counters.len() >= MAX_STATE_ENTRIES {
            return Err(Error::Capacity);
        }
        state.emit(
            "metric.increment",
            props([("name", json!(name)), ("amount", json!(amount))]),
            None,
        )?;
        let aggregate = state
            .aggregates
            .counters
            .entry(name.to_owned())
            .or_default();
        aggregate.count = aggregate.count.saturating_add(1);
        aggregate.sum += amount;
        Ok(())
    }

    pub fn screen_enter(&self, id: &str) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? {
            return Ok(());
        }
        validate_id(id).map_err(|_| Error::InvalidInput("invalid screen identifier"))?;
        if state.screens.contains_key(id) {
            return Err(Error::InvalidInput("screen is already entered"));
        }
        if state.screens.len() >= MAX_STATE_ENTRIES {
            return Err(Error::Capacity);
        }
        let running = state.running_time();
        state.emit("screen.enter", props([("id", json!(id))]), None)?;
        state.screens.insert(id.to_owned(), running);
        Ok(())
    }

    pub fn screen_leave(&self, id: &str) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? {
            return Ok(());
        }
        let entered = *state
            .screens
            .get(id)
            .ok_or(Error::InvalidInput("screen is not entered"))?;
        let duration = state.running_time().saturating_sub(entered);
        state.emit(
            "screen.leave",
            props([("id", json!(id)), ("duration_ms", json!(duration))]),
            None,
        )?;
        state.screens.remove(id);
        Ok(())
    }

    pub fn workflow_start(&self, name: &str) -> Result<String, Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? {
            return Ok(String::new());
        }
        validate_id(name).map_err(|_| Error::InvalidInput("invalid workflow identifier"))?;
        if state.workflows.len() >= MAX_STATE_ENTRIES {
            return Err(Error::Capacity);
        }
        let id = new_id();
        state.emit(
            "workflow.start",
            props([("name", json!(name))]),
            Some(id.clone()),
        )?;
        state.workflows.insert(id.clone(), Instant::now());
        Ok(id)
    }

    pub fn workflow_step(&self, id: &str, step: &str) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? || id.is_empty() {
            return Ok(());
        }
        if !state.workflows.contains_key(id) {
            return Err(Error::InvalidInput("unknown workflow"));
        }
        state.emit(
            "workflow.step",
            props([("step", json!(step))]),
            Some(id.to_owned()),
        )
    }

    pub fn workflow_end(&self, id: &str, outcome: WorkflowOutcome) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? || id.is_empty() {
            return Ok(());
        }
        let started = state
            .workflows
            .get(id)
            .ok_or(Error::InvalidInput("unknown workflow"))?;
        let duration = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        state.emit(
            "workflow.end",
            props([
                ("outcome", json!(outcome)),
                ("duration_ms", json!(duration)),
            ]),
            Some(id.to_owned()),
        )?;
        state.workflows.remove(id);
        Ok(())
    }

    pub fn span(&self, name: &str) -> Result<Span, Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::Performance)? {
            return Ok(Span {
                sdk: None,
                name: None,
                started: None,
                epoch: 0,
            });
        }
        validate_id(name).map_err(|_| Error::InvalidInput("invalid span identifier"))?;
        Ok(Span {
            sdk: Some(self.clone()),
            name: Some(name.to_owned()),
            started: Some(Instant::now()),
            epoch: state.consent_epoch,
        })
    }

    /// Rejects a nonfinite duration or sum before enqueueing or changing any histogram field.
    pub fn record_span(&self, name: &str, duration_ms: f64) -> Result<(), Error> {
        self.state()?.record_span(name, duration_ms, None)
    }

    pub fn lifecycle(&self, window_id: &str, visible: bool, focused: bool) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? {
            return Ok(());
        }
        validate_id(window_id).map_err(|_| Error::InvalidInput("invalid window identifier"))?;
        let now = state.tick();
        if !state
            .lifecycle
            .as_mut()
            .expect("enabled session has lifecycle")
            .window(window_id, visible, focused, now)
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }

    pub fn interaction(&self) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? {
            return Ok(());
        }
        let now = state.tick();
        state
            .lifecycle
            .as_mut()
            .expect("enabled session has lifecycle")
            .interaction(now);
        Ok(())
    }

    pub fn suspend(&self, suspended: bool) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? {
            return Ok(());
        }
        let now = state.tick();
        state
            .lifecycle
            .as_mut()
            .expect("enabled session has lifecycle")
            .suspend(suspended, now);
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<(), Error> {
        let mut state = self.state()?;
        if !state.eligible(Category::UsageSession)? {
            return Ok(());
        }
        let now = state.tick();
        let lifecycle = state
            .lifecycle
            .as_mut()
            .expect("enabled session has lifecycle");
        lifecycle.advance(now);
        let totals = lifecycle.totals();
        state.emit(
            "lifecycle.checkpoint",
            props([
                ("running_ms", json!(totals.running_ms)),
                ("foreground_visible_ms", json!(totals.foreground_visible_ms)),
                ("focused_ms", json!(totals.focused_ms)),
                ("interaction_active_ms", json!(totals.interaction_active_ms)),
                ("idle_ms", json!(totals.idle_ms)),
            ]),
            None,
        )
    }

    pub fn flush(&self, timeout: Duration) -> Result<(), Error> {
        self.flush_inner(timeout, false)
    }

    fn flush_inner(&self, timeout: Duration, allow_closing: bool) -> Result<(), Error> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(Error::InvalidInput("invalid flush timeout"))?;
        let mut state = self.state()?;
        if !allow_closing {
            state.open()?;
        }
        if state.queue.is_empty() {
            return Ok(());
        }
        if self.inner.transport.is_none() {
            return Err(Error::NoTransport);
        }
        let initial_attempt = state.send_attempt;
        state.requested = true;
        state.flush_deadline = Some(
            state
                .flush_deadline
                .map_or(deadline, |old| old.min(deadline)),
        );
        self.inner.changed.notify_all();
        loop {
            if state.queue.is_empty() {
                return Ok(());
            }
            if state.stopped {
                return Err(Error::Closed);
            }
            if state.send_attempt > initial_attempt && state.last_send_error == Some(false) {
                return Err(Error::Transport { retryable: false });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if state.flush_deadline == Some(deadline) {
                    state.flush_deadline = None;
                }
                return Err(Error::DeadlineExceeded);
            }
            let (next, _) = self
                .inner
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| Error::Poisoned)?;
            state = next;
        }
    }

    pub fn shutdown(&self, timeout: Duration) -> Result<(), Error> {
        {
            let mut state = self.state()?;
            if state.stopped {
                return Ok(());
            }
            state.closing = true;
        }
        let result = self.flush_inner(timeout, true);
        let mut state = self.state()?;
        state.stopped = true;
        state.closing = true;
        self.inner.changed.notify_all();
        result
    }

    pub fn diagnostics(&self) -> Diagnostics {
        self.state()
            .map(|state| state.diagnostic_snapshot())
            .unwrap_or(Diagnostics {
                closed: true,
                ..Diagnostics::default()
            })
    }

    pub fn aggregates(&self) -> Aggregates {
        self.state()
            .map(|state| state.aggregates.clone())
            .unwrap_or_default()
    }

    pub fn capabilities(&self) -> Value {
        json!({
            "schema_version": 1,
            "sdk_version": SDK_VERSION,
            "privacy_enforcement": true,
            "memory_queue": true,
            "sqlite": cfg!(feature = "sqlite"),
            "http": cfg!(feature = "http"),
            "configured_transport": self.inner.transport.is_some(),
            "normalized_lifecycle": true,
            "automatic_platform_lifecycle": false,
            "encrypted_persistence": false,
            "native_crash_capture": false,
            "crash_session_recovery": false,
            "automatic_process_metrics": false,
            "browser_indexeddb": false
        })
    }
}

/// A span records at most once. A revoked/regranted consent epoch invalidates it.
pub struct Span {
    sdk: Option<Telemetry>,
    name: Option<String>,
    started: Option<Instant>,
    epoch: u64,
}

impl Span {
    fn emit_once(&mut self) -> Result<(), Error> {
        let Some(sdk) = self.sdk.take() else {
            return Ok(());
        };
        let Some(name) = self.name.take() else {
            return Ok(());
        };
        let Some(started) = self.started.take() else {
            return Ok(());
        };
        let mut state = sdk.state()?;
        if state.closing
            || state.stopped
            || state.consent_epoch != self.epoch
            || !state.gate.enabled(Category::Performance)
        {
            return Ok(());
        }
        state.record_span(
            &name,
            started.elapsed().as_secs_f64() * 1_000.0,
            Some(self.epoch),
        )
    }

    pub fn finish(mut self) -> Result<(), Error> {
        self.emit_once()
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let _ = self.emit_once();
    }
}
