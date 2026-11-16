# Support and implementation limits

Version 0.1 is an unpublished, source-built SDK. Capability discovery describes the implementation available to an instance; callers should check it before enabling optional instrumentation. A successful build on one host does not establish support for other operating systems or CPU architectures.

| Target | Integration | Delivery and limits |
|---|---|---|
| Rust | Direct `telemetry-rust` crate, library name `telemetry` | Native engine; optional HTTP, SQLite and tracing integrations. UI toolkit supplies normalized lifecycle state. |
| C/C++ | Versioned `telemetry_v1_*` ABI | Build `telemetry-ffi`; opaque numeric handles and owned response buffers. No prebuilt libraries. |
| Python | PyO3 extension and Python package | Source build using Cargo or maturin. Optional logging handler; explicit lifecycle calls. No published wheels. |
| Node.js | napi-rs addon and ESM wrapper | Source-built addon. Initialization and every public method dispatch native work asynchronously and return promises. No published platform binaries. |
| Go | cgo wrapper over the C ABI | Requires cgo, a C toolchain and a matching native library. Pure-Go fallback is not implemented. |
| Dart/native Flutter | `dart:ffi` wrapper over the C ABI | Requires a matching native library. Application supplies lifecycle and screen events. Mobile binary packaging and automatic Flutter lifecycle plugins are not provided. |
| Browser | wasm-bindgen subset and JavaScript adapter | Shared schema/privacy/queue logic, memory queue and explicit export. No session/lifecycle, screen or workflow tracking, SQLite, OS process metrics or IndexedDB persistence. |

## Shared native behavior

Native bindings delegate event validation, consent, error redaction and bounded queue handling to Rust. The engine supports typed actions and errors, counters, spans, screen entry/leave, workflows, explicit lifecycle events, diagnostics and manual flush/shutdown. It does not infer input content or install global keyboard hooks.

OTLP/HTTP JSON is the default network protocol. Native and browser exporters use the shared Rust mapping for logs, delta metrics and standalone traces. They can target an existing OpenTelemetry Collector, including the collector used by ClickStack. The previous custom batch API remains available only through `export_protocol: "legacy_json"`. See [setup and signal mapping](opentelemetry.md).

This is OTLP export compatibility. It does not install OpenTelemetry auto-instrumentation, implement the full OpenTelemetry SDK API, propagate W3C trace context, or create parent/child traces across services. It does not deploy a collector or ClickStack instance.

The native `aggregates()` API exposes bounded local counter and span snapshots. Counter and span maps each allow 256 names and are cleared when the corresponding consent is withdrawn. This local API does not complete the design's later server analytics work.

Rust, Python and Go APIs can perform synchronous SQLite I/O. Node offloads all native calls; Dart offloads them to an isolate. Framework integration should account for these calling-thread differences.

Lifecycle flags come from the application's UI framework. Running, foreground/visible, focused, interaction-active and idle time have separate meanings. A focused window alone is not evidence of interaction. App-level totals account for multiple windows without adding overlapping time twice.

The `telemetry-platform` crate's optional `process-metrics` feature reads Linux current-process RSS and cumulative CPU clock ticks. CPU ticks are not CPU percent or seconds; converting them requires the host's clock-tick rate. No automatic sampling is installed. CPU utilization, GPU, energy, native crash dumps and automatic window detection are unsupported.

## Work outside this implementation

The design's full definition of done includes services and production validation beyond a client library. This repository does not establish those milestones:

- A deployed collector or ClickStack instance, ClickHouse storage, retention/deletion services, dashboards or read-only MCP.
- Signed releases, published packages, complete OS/architecture build matrices, mobile store distribution or measured battery/performance budgets.
- Native crash markers, partial-session restoration after abnormal exit, minidumps or encrypted persistence.
- Pure-Go collection without cgo, automatic Flutter plugin instrumentation or browser IndexedDB persistence.
- A guarantee that termination, page close or mobile suspension uploads pending events.

Do not report these as supported based on the presence of an API or a sample. Platform validation evidence should name the exact build, runtime and command used.
