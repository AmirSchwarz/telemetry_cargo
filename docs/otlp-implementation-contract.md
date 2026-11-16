# OTLP implementation contract

This change adds OTLP/HTTP JSON using the existing Serde and Reqwest dependencies. No new runtime dependency is needed. The existing private JSON protocol remains available as an explicit legacy option.

## Shared configuration

`ExportProtocol` (in telemetry-transport, reexported by core) derives Clone/Copy/Debug/Default/Serialize/Deserialize/PartialEq/Eq with snake_case values `otlp_http_json` (default) and `legacy_json`.

Core Config adds `export_protocol:ExportProtocol`, `otlp_headers:BTreeMap<String,String>` (default empty), and `allow_insecure_loopback:bool` (default false). The endpoint for OTLP is a collector base URL, such as `https://collector.example.com` or `https://collector.example.com/otlp`; the exporter appends `/v1/logs`, `/v1/metrics`, `/v1/traces`. HTTPS is mandatory except an explicit loopback opt-in for literal 127.0.0.1 or ::1. `localhost` does not receive the plaintext exception. Reject URLs with credentials/query/fragment, signal suffixes and unexpected malformed path segments. Legacy retains its existing exact endpoint and bearer-token behavior; reject OTLP-only options in legacy mode.

`ingestion_token` continues to mean `Authorization: Bearer <token>`. ClickStack's ingestion key is instead configured as `otlp_headers: {"authorization":"<public ingestion API key>"}` (raw value, no forced Bearer). A duplicate explicit authorization plus ingestion_token is rejected. Header names/values/total length are bounded, hop-by-hop/content/cookie/host headers forbidden, secrets omitted from Debug and errors. Do not read environment variables implicitly; examples may explicitly read existing TELEMETRY_* variables.

## Delivery and shared encoder

telemetry-transport exports:

* `DeliveryReport { consumed:Vec<String>, rejected:usize, error:Option<TransportError> }`. Consumed means remove from queue (accepted or definitively rejected); rejected counts lost event records. Existing `Transport::send` remains required; a new default `send_report` wraps it and consumes all IDs only on success. Existing third-party transports therefore continue to compile.
* `OtlpSignal::{Logs,Metrics,Traces}` serde snake_case. `OtlpSignal::path()->&'static str` and `rejected_field()->&'static str` return standard endpoint and response field.
* `OtlpRequest { signal:OtlpSignal, path:String, event_ids:Vec<String>, body:serde_json::Value }` derives Serialize.
* `otlp_requests(events:&[Event])->Result<Vec<OtlpRequest>,TransportError>` is thread/network-free, compiled without the http feature and reused by WASM. Requests ordered Logs, Metrics, Traces, omit empty signals. Each event appears in exactly one request.
* `OtlpHttpTransport::new(endpoint:&str,token:Option<String>,headers:BTreeMap<String,String>,allow_insecure_loopback:bool)->Result<Self,TransportError>` under http. Implements send_report; send simply reports error when any signal failed. Lazy client creation on worker.

Map action/error/screen/workflow/lifecycle/custom events to LogRecords. Errors severity17/ERROR, others9/INFO. Body is semantic event name; properties become `telemetry.property.<key>` attributes; category/schema version/event ID and optional permitted session/workflow IDs use `telemetry.*` attributes. Raw error text stays redacted by the existing engine. No device/user IDs are added.

Map span to a standalone root OTLP span with name from properties.name, kind1/INTERNAL, deterministic valid traceId (event UUID without hyphens) and spanId (last16 hex chars), endTimeUnixNano=occurred_at*1_000_000, start=end-duration_ms*1_000_000 clamped at0. No fabricated distributed parent context. Numeric enum fields; timestamps/int64 values decimal strings, trace/span IDs hex as OTLP JSON requires.

Map metric.increment to a NumberDataPoint in a Sum metric named `telemetry.<name>`, aggregationTemporality1/DELTA, isMonotonic=false (signed increments are supported), asDouble=amount. Group same resource/name into one metric; one datapoint per event. Do not invent a stream start time; omit optional startTimeUnixNano. These are discrete deltas, not a continuously aggregated OpenTelemetry SDK metric stream. Never attach event/session/workflow IDs to metric labels. Attributes may contain category only. Do not attach sensitive/unique properties as metric dimensions.

Group resources by app_id/app_version/platform/sdk_version. Resource attributes: service.name, service.version, telemetry.sdk.name="telemetry-cargo", telemetry.sdk.language="rust", telemetry.sdk.version, telemetry.platform. Scope name="telemetry-cargo", version=sdk_version. Preserve finite scalar property types with OTLP AnyValue. Encoding is bounded and checked; validate known builtin data and time arithmetic rather than silently emitting malformed OTLP.

## HTTP/queue semantics

Only HTTP200 with valid OTLP JSON response acknowledges delivery. Empty JSON object is success. Parse response at most64KiB including after decompression; no raw response message is propagated. Shared network-free `parse_otlp_response(body:&[u8],signal:OtlpSignal,submitted:usize)` parses partialSuccess/rejectedLogRecords or rejectedDataPoints or rejectedSpans under ProtoJSON int64 rules: integral numeric and quoted numeric forms, including exponents, and null-as-unset. Use Serde JSON's raw_value feature to preserve exact number tokens and reject fractional/underflow values rather than rounding them. WASM exposes this same helper to the browser adapter. Zero warning is accepted; positive count means consumed without retry and reported as permanent export failure with rejected count. Unknown response fields are ignored. Impossible counts/malformed success responses are reported safely and never treated as full success.

Only 429/502/503/504 and transient connection/timeouts are retryable per OTLP. Honor Retry-After. Definitive nonretryable responses consume the affected signal IDs with rejected count. Already consumed signals must not be resent after another signal fails. Requests share one total send timeout and 4MiB aggregate serialized payload budget. No redirects, cookies, proxy credentials or body echo.

Worker uses send_report, validates consumed IDs subset/uniqueness and rejected<=consumed.len(), removes consumed IDs even alongside an error, tracks rejected in dropped diagnostics, and retries only remaining rows. For a flush attempt, a permanent rejection must be returned before testing queue-empty, so partial failure cannot be reported as successful flush. Consent upload gate and privacy revalidation stay in force across all signals.

## Browser and bindings

Native C/Python/Node/Dart use core Config without ABI change. Go typed Config and TS Config gain these fields (Go export protocol constants). Browser adapter snapshots protocol/headers/loopback flag and strips adapter-only fields before WASM Config parse. WASM adds `otlp_batch {limit}` yielding shared `OtlpRequest` objects from the consent-filtered queue. Browser posts signal requests with per-signal acknowledgement and same HTTP/partial/deadline rules. Before every request and after every await, use consent generation/AbortController checks to prevent sends from stale prepared batches after withdrawal. Retry payloads live only in Rust queue. Legacy browser behavior remains explicit. Unsupported protocol names fail early.

## Verification

Unit/schema fixtures, real local HTTP tests (including auth, response errors, partial acceptance, size limits, timeouts), core retry/partial-ack/consent tests, rebuilt bindings + actual WASM tests. Run a real official OpenTelemetry Collector receiver, send from the SDK, inspect accepted logs/metrics/traces in its file/debug exporter. Provide runnable collector/ClickStack configuration and exact integration instructions. Do not claim a complete OpenTelemetry SDK API or distributed trace context propagation; this is standards-compatible OTLP export with the existing SDK privacy controls. Production TLS/auth remain application/deployment choices.
