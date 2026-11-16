# Canonical event contract

`registry-v1.json` is compiled directly into the Rust schema registry. All native
language adapters call the Rust engine and therefore share its categories, event
names, property rules and consent behavior. Bindings must not accept arbitrary
properties independently of this registry.

Run `python3 schemas/generate.py` after editing canonical assets. It vendors the
registry and golden fixture under `crates/telemetry-schema/schema/`, keeping the
Rust crate self-contained for packaging. `python3 schemas/generate.py --check`
detects drift; edit canonical files here rather than generated crate assets.

`envelope-v1.schema.json` specifies the structural wire envelope. A gateway must
also apply the event registry rules, finite-number checks, byte budgets and app
quotas; JSON Schema alone does not enforce the complete policy. Timestamps are
Unix milliseconds, never wall-clock duration calculations. UUIDs are random,
ephemeral IDs rather than device identities.

`session_id` and `workflow_id` are permitted only on `usage_session` records.
Diagnostics, aggregate usage and every other category reject these fields even
when their values are valid random UUIDs, preventing independent consent scopes
from retaining session linkage through restored records.

Rules encode as strings (`identifier`, `number`, `boolean`, `redacted_text`) or
`{"enum":["allowed","values"]}`. Redacted text must be exactly `[redacted]` on
the wire; arbitrary messages are never persisted. Semantic IDs must come from
application code and must not be derived from user content.

Registry changes that alter category or property semantics require a new schema
version and migration. Custom event definitions exist only for the lifetime of a
configured engine; SQLite restoration discards unknown definitions rather than
trusting a stale policy. Built-in definitions cannot be replaced.

The SQLite queue permits one live owner per database file. Use a separate private
file per app instance/profile. On Unix a new file is created with mode `0600`;
existing group/world-accessible files and symlink paths are rejected. Provision
private directory permissions and Windows ACLs in the host app. The payload
budget bounds retained JSON bytes; SQLite pages, indexes, WAL and filesystem
allocation add overhead. This queue does not encrypt payloads.
