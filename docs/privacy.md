# Privacy and data handling

## Consent

Every category defaults to disabled: `essential_operations`, `diagnostics`, `performance`, `usage_aggregate`, `usage_session` and `extended_support`. Essential operations also require `essential_operations_reviewed = true`; that configuration flag records an application decision and does not establish a legal basis.

The Rust engine checks permission before collection, retained session state, queue persistence and upload. Disabled collection succeeds without retaining the event. An empty workflow ID means workflow collection was disabled.

Withdrawing consent purges queued events in that category. Withdrawing session consent also clears screen, workflow and lifecycle state. Granting it again starts a new random session. Withdrawal synchronizes with export; it cannot retract requests already sent or remove data already received by a server. Server retention and deletion must be implemented by the gateway operator.

Consent withdrawal, flush and shutdown may wait for exporter work. Collection never performs a network request on the calling thread, but native SQLite initialization and collection perform synchronous local disk I/O and can wait for locks. Rust, Python and Go applications should schedule that work outside a UI thread. Releasing Python's GIL does not make its calling thread asynchronous. Node dispatches initialization and all public methods through asynchronous N-API tasks; Dart uses a dedicated isolate.

Local counter and span aggregates are also consent-gated. Withdrawing their respective usage/performance category clears the aggregate map. These aggregates are not automatically anonymous or a substitute for a retention policy.

## Allowed data

Use fixed semantic names such as `search.submit` and `document.import`, not user input. Event names must be registered. Registered fields have a type rule; unknown fields, arrays, objects and null values are rejected. Identifiers use ASCII letters, digits, `.`, `_` and `-`, with a maximum length of 96 bytes. This syntax restriction does not make user-derived values safe: an email fragment or account number can still fit it.

The standard error API retains a controlled error code and replaces the entire raw message with `[redacted]`. It does not attempt to preserve free text with a PII-detection regex. The tracing bridge exports only explicitly supplied `telemetry_code` values on error events; the Python logging handler uses the fixed code `python.logging`. Logging text, paths and tracebacks are not exported by those adapters.

Do not put credentials, usernames, device IDs, file paths, document content, search terms or URLs in semantic identifiers, enum values, metric names or application metadata. Custom registry definitions are application code and need the same review as built-in definitions.

Event IDs are random. Session IDs and workflow IDs are ephemeral random identifiers. Events also contain app ID/version, SDK version, platform, category and a wall-clock Unix-millisecond timestamp. Durations use monotonic clocks. Session aggregates can still be identifiable; they are not automatically anonymous.

## Local storage

The default queue is in memory. Queue defaults are 1,024 events, 4 MiB of serialized event data, a 24-hour TTL and 512 events per category. Event defaults allow 32 properties, 128-byte strings and an 8,192-byte encoded event. Configure smaller budgets where appropriate.

Native SQLite storage is opt-in through `sqlite_path` and requires the `sqlite` feature. It uses SQLite WAL. Each database has one SDK owner at a time; give independent SDK instances separate queue files. SQLite storage is **not encrypted** by this SDK. Logical queue byte limits are not an exact limit on database/WAL file size; operating-system permissions and disk protection remain application responsibilities. Do not put the queue on a shared or publicly readable path.

At startup the native engine filters restored data using current consent, the current app ID, configured limits and the built-in registry. Unknown or invalid events are discarded. Custom definitions are registered after initialization, so custom events from an earlier process are discarded on restoration. Custom definitions are not persisted; do not rely on them for delivery across restarts.

The browser subset currently uses memory storage; IndexedDB persistence is not implemented. Closing a page can lose unsent data. Mobile suspension and process termination likewise prevent guarantees about a final upload.

## Transport and backend responsibilities

The HTTP transport accepts the full `https://…/v1/telemetry/batches` endpoint, rejects URL credentials, query strings and fragments, and does not follow redirects. Only 2xx responses acknowledge a batch. Retryable failures keep events queued; delivery may repeat, so the gateway must deduplicate by event ID. `Retry-After` supports delta-seconds; HTTP-date parsing is not implemented.

Use public per-app ingestion tokens, never administration credentials. Transport errors omit tokens and response bodies. This does not control reverse-proxy access logs or prevent servers from seeing source IP addresses.

The gateway still needs payload validation, authentication, rate limits, quotas, replay protection, retention and deletion controls. The SDK does not deploy those services, perform a legal assessment, encrypt SQLite, install fatal-signal handlers or collect minidumps. Consult the original design for those separate requirements.
