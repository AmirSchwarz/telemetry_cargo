# Implementation contract (0.1)

This file fixes the internal interfaces for the initial implementation of the design. The original design remains the longer-term roadmap. Source is licensed under Apache License 2.0; packages are unpublished pending namespace and release decisions.

## Shared schema

`telemetry-schema` exports serde snake_case `Category` (the six categories in the design, deriving Copy/Ord/Hash), `Properties = BTreeMap<String, serde_json::Value>`, `Event`, `Registry`, `EventDefinition`, `PropertyRule`, `Limits`, `SchemaError`, `validate_id(&str) -> Result<(), SchemaError>`, `now_unix_ms() -> u64`, `new_id() -> String`.

`Event` fields: schema_version:u32 (1), event_id:String (UUID v4), app_id:String, app_version:String, platform:String, sdk_version:String, category:Category, event_name:String, occurred_at:u64 (Unix milliseconds), properties:Properties, session_id:Option<String>, workflow_id:Option<String>. Optional IDs omitted when None.

`Registry::default()` supplies builtin events; `register(EventDefinition) -> Result<(), SchemaError>` refuses replacement; `definition(&str) -> Option<&EventDefinition>`; `validate(&Event, &Limits) -> Result<(), SchemaError>`. `EventDefinition { name:String, category:Category, properties:BTreeMap<String, PropertyRule> }`. `PropertyRule` variants: Identifier, Number, Boolean, Enum(Vec<String>), RedactedText. All property rules optional, unknown keys rejected. RedactedText is replaced by `[redacted]` before validation or storage. Identifier is a bounded semantic identifier, never free text. Limits fields: max_properties:usize (32), max_string_bytes:usize (128), max_event_bytes:usize (8192). Names and identifiers: ASCII alphanumeric plus `.`, `_`, `-`, length 1..=96. Numeric values must be finite.

Builtin event properties (Identifier unless noted):

* action / UsageAggregate: id, result enum(success,failure,cancelled)
* error / Diagnostics: code, message RedactedText
* metric.increment / UsageAggregate: name, amount Number
* span / Performance: name, duration_ms Number
* screen.enter / UsageSession: id
* screen.leave / UsageSession: id, duration_ms Number
* workflow.start / UsageSession: name
* workflow.step / UsageSession: step
* workflow.end / UsageSession: outcome enum(complete,cancelled,failed,timeout), duration_ms Number
* lifecycle.checkpoint / UsageSession: running_ms, foreground_visible_ms, focused_ms, interaction_active_ms, idle_ms (all Number)

`telemetry-privacy` exports `Consent` (default all false; `enabled(Category)->bool`, `set(Category,bool)`), `PrivacyGate::new(consent:Consent, essential_operations_reviewed:bool)`, `enabled(Category)->bool`, `set_consent(Category,bool)`, `allowed_categories()->BTreeSet<Category>`, `sanitize(&EventDefinition, &mut Properties)` (redacts RedactedText). Essential operations additionally require reviewed=true.

## Queue

`telemetry-queue`: `QueueConfig { max_events:usize (1024), max_bytes:usize (4MiB), ttl_ms:u64 (86400000), per_category_max_events:usize (512) }` Default + Serialize/Deserialize. `QueueError` implements Error.

`trait EventQueue: Send { fn enqueue(&mut self, event:Event, now_ms:u64)->Result<usize,QueueError>; fn batch(&mut self, limit:usize, now_ms:u64)->Result<Vec<Event>,QueueError>; fn acknowledge(&mut self, ids:&[String])->Result<(),QueueError>; fn retain_categories(&mut self, allowed:&BTreeSet<Category>)->Result<(),QueueError>; fn len(&self)->usize; fn byte_len(&self)->usize; fn is_empty(&self)->bool { self.len()==0 } }`. Enqueue returns evicted/expired count. batch never deletes unacknowledged records. MemoryQueue::new(config)->Result<Self,QueueError>. Optional feature sqlite: SqliteQueue::open(path:impl AsRef<Path>,config)->Result<Self,QueueError>. Startup validates persisted envelopes, bounds and TTL. Fail closed on storage errors.

## Transport

`telemetry-transport` exports `trait Transport: Send + Sync { fn send(&self, events:&[Event], timeout:Duration)->Result<(),TransportError>; }`, `TransportError { message:String, retryable:bool, retry_after:Option<Duration> }` implementing Error. No body/token echoed in errors. Default trait has no network dependencies. `HttpTransport::new(endpoint:&str,token:Option<String>)->Result<Self,TransportError>` under `http` feature uses reqwest blocking on exporter worker only; HTTPS, no userinfo/fragment/query, no redirects, no raw response bodies. Explicit `for_loopback_test(endpoint,token)` permits only 127.0.0.1/[::1] for synthetic tests. Endpoint is full /v1/telemetry/batches URL. Batch JSON `{schema_version:1,events:[...]}`. Treat only 2xx as ack; retry 408,429,5xx and connection failures; honor Retry-After seconds.

`telemetry-platform` capability reporting and current-process resource sampling only; no application-global hooks. No raw paths or identifiers in resource measurements.

## Core and facade

`telemetry-core` exports `Config` serde default + deny_unknown_fields: app_id:String, app_version:String, endpoint:Option<String>, ingestion_token:Option<String>, consent:Consent, essential_operations_reviewed:bool, queue:QueueConfig, limits:Limits, idle_threshold_ms:u64, sqlite_path:Option<String>, batch_size:usize, flush_interval_ms:u64. `Config::new(app_id, app_version)`. Config debug must redact ingestion token/path. Defaults: optional categories off, memory only, no exporter without endpoint. Endpoint when present starts background worker; API collection never performs network. Optional sqlite must be explicitly enabled; otherwise requested sqlite returns error. Core features `http`, `sqlite`; facade default features http, sqlite, tracing.

`Telemetry` Clone + Send + Sync; `Telemetry::init(Config)->Result<Self,Error>`, `with_transport(Config, Arc<dyn Transport>)->Result<Self,Error>` for synthetic tests. Methods use `&self`:

* `set_consent(Category,bool)->Result<(),Error>`
* `event(&str,Properties)->Result<(),Error>`; `register_event(EventDefinition)->Result<(),Error>`
* `action(&str,ActionResult)->Result<(),Error>` (Success/Failure/Cancelled)
* `capture_error(&str,&str)->Result<(),Error>` (code, raw message; always redact)
* `increment(&str,f64)->Result<(),Error>`
* `screen_enter(&str)->Result<(),Error>`; `screen_leave(&str)->Result<(),Error>`
* `workflow_start(&str)->Result<String,Error>`; `workflow_step(&str,&str)->Result<(),Error>`; `workflow_end(&str,WorkflowOutcome)->Result<(),Error>`
* `span(&str)->Result<Span,Error>`; Span::finish(self)->Result<(),Error>, Drop emits best-effort once
* `lifecycle(window_id:&str, visible:bool, focused:bool)->Result<(),Error>`; `interaction()->Result<(),Error>`; `suspend(bool)->Result<(),Error>`; `checkpoint()->Result<(),Error>`
* `flush(Duration)->Result<(),Error>`; `shutdown(Duration)->Result<(),Error>`; `diagnostics()->Diagnostics`; `capabilities()->serde_json::Value`.

Unknown custom events rejected until registration. Denied collection returns Ok and retains no event, span, screen, workflow or lifecycle state. Workflow start when denied returns empty String, subsequent empty-ID operations no-op. Registry and state maps bounded (256 entries). Usage-session consent creates random ephemeral session; withdrawal clears all session state + queued events; regrant starts fresh. Consent withdrawal synchronizes with exporter so no denied batch starts after it returns (cannot retract already sent requests). Flush without transport returns error if queue has data and leaves queue intact; returns success if empty. Deadline honored for network retries; retry state holds event IDs in queue, not duplicate raw payloads. SDK diagnostics expose bounded counters not raw payloads. Shutdown stops background exporter and gates future collection.

Lifecycle uses monotonic `Instant`, per-window flags and app-level union; active only with foreground-visible-focused plus input within threshold; split elapsed at idle boundary. Initial state no focused windows and no input. Suspension excludes wall gap from running/foreground/etc. `Lifecycle` pure helper with injected millisecond ticks for deterministic tests is encouraged.

## Binding protocol

`telemetry-core::Telemetry::command(&self, command:serde_json::Value)->Result<serde_json::Value,Error>` shared JSON dispatch for bindings. Tagged `op` with the above snake_case method names. `action {id,result}`, `capture_error {code,message}`, `increment {name,amount}`, `event {name,properties}`, `register_event {definition}`, `set_consent {category,enabled}`, `screen_enter/leave {id}`, `workflow_start {name}` -> `{id}`, `workflow_step {id,step}`, `workflow_end {id,outcome}`, `span {name,duration_ms}` (external adapter duration), `lifecycle {window_id,visible,focused}`, `interaction`, `suspend {suspended}`, `checkpoint`, `flush/shutdown {timeout_ms}`, `diagnostics`, `capabilities`. Successful void returns `{}`. Strict parsing, unknown operations/fields rejected. All language entrypoints use this dispatcher and expose native errors.

C ABI crate `telemetry-ffi` emits cdylib/staticlib/rlib: `telemetry_v1_init(config_ptr:*const u8,config_len:usize,out:*mut u64)->i32`, `telemetry_v1_command(handle:u64,request_ptr:*const u8,request_len:usize,out:*mut TelemetryBuffer)->i32`, `telemetry_v1_destroy(handle:u64)->i32`, `telemetry_v1_buffer_free(buffer:TelemetryBuffer)`. Buffer repr(C): data:*mut u8,len:usize. All outputs initialized on entry, error JSON contains generic message without inputs. Return 0 success, 1 invalid input, 2 invalid handle, 3 SDK error, 4 panic. Handles are opaque monotonically allocated registry IDs, so stale/double-destroy IDs safely rejected. Each returned buffer freed exactly once by matching library. Valid pointers with lengths required; impossible to validate arbitrary dangling addresses. Panic catch at boundaries. Command returns buffer even on SDK error. Header documents ABI, ownership and concurrency.

Python PyO3 exposes NativeTelemetry(config_json:string), command(request_json:string)->string, close(). The Python package wraps it; no ctypes fallback is shipped. Node NAPI uses AsyncTask for initialization and every public operation, including collection, diagnostics and close. This keeps SQLite operations off the event loop. Go cgo and Dart FFI use the header; Dart performs native work in a dedicated isolate. No silent fallback to alternate semantics. The browser WASM subset advertises missing transport/persistence/session features; it does not bundle std threads or SQLite.

## Finalized additions and limits

* `Diagnostics` includes effective `consent` and `consent_epoch`. External span commands accept optional `consent_epoch`; adapters capture permission and epoch at span start and omit measurements if consent changes. Python context managers preserve an application's original exception when telemetry cleanup fails.
* `Telemetry::aggregates()` / command `aggregates` returns bounded counter count/sum and span count/sum/min/max/histogram snapshots. Span upper-inclusive boundaries are 1, 5, 10, 50, 100, 500, 1000 and 5000ms, followed by an overflow bucket. Bins are individual counts. Arithmetic overflow rejects the new sample before event collection or aggregate mutation. Category withdrawal clears corresponding aggregates.
* Optional session/workflow IDs are accepted only for usage_session events, including persisted records. SQLite startup rejects invalid linkage and another app's records. Canonical registry and fixture assets are vendored mechanically with `python3 schemas/generate.py`; `--check` detects drift.
* Lifecycle checkpoints are explicit cumulative snapshots, so queries use latest values or deltas. Screen dwell counts process-running time, excludes suspension, and is separate from idle/activity totals. Workflow elapsed durations include suspension.
* Native Rust, Python and Go collection performs synchronous local SQLite I/O when persistence is selected. Those callers must offload it when latency matters. Memory collection performs no disk or network I/O. Node and Dart public adapters already offload native calls.
* Browser scope is memory-buffered actions, errors, increments and spans with explicit HTTPS fetch export. IndexedDB, lifecycle/screens/workflows, periodic export and page-exit guarantees are unsupported and reported as such. Concurrent flush callers retain their own deadlines.

## Validation and scope

Cargo test/fmt/clippy; actual C/Python/Node smoke tests; Go and Dart when toolchains work. This implementation does not deploy a gateway or claim the later ClickHouse/dashboard/MCP/signing/mobile/crash-hardening phases are complete. A synthetic gateway demonstrates protocol acceptance without production claims. Shared schema/fixtures and package usage examples are required.
