# Integration guide

Run build commands from the repository root unless a section says otherwise. Examples omit an endpoint so they can run without a server. Categories must be enabled explicitly. For remote delivery, set the base URL of an existing OpenTelemetry Collector or ClickStack collector. The default protocol is `otlp_http_json`; see [OpenTelemetry and ClickStack setup](opentelemetry.md) for endpoints, authentication, local testing and migration from the legacy JSON protocol.

## Rust

See the root README for the local Cargo dependency. The runnable sample is:

```sh
cargo run -p telemetry-rust --example basic
```

The sample reads `TELEMETRY_ENDPOINT` and `TELEMETRY_INGESTION_TOKEN`. Set the endpoint to a collector base URL, such as `https://collector.example.com`. The token is sent as Bearer authentication. Without an endpoint the sample leaves its demonstration events in the in-memory queue until process exit. These environment variables are read explicitly by the sample; the SDK does not read exporter environment variables implicitly.

To measure a span:

```rust
sdk.set_consent(telemetry::Category::Performance, true)?;
{
    let _span = sdk.span("document.ocr")?;
    // Run the measured operation.
}
```

For session and workflow instrumentation:

```rust
sdk.set_consent(telemetry::Category::UsageSession, true)?;
sdk.lifecycle("main", true, true)?;
sdk.interaction()?; // Signal occurrence only; never supply the actual input.
sdk.screen_enter("archive.search")?;
let id = sdk.workflow_start("document.import")?;
sdk.workflow_step(&id, "files_selected")?;
sdk.workflow_end(&id, telemetry::WorkflowOutcome::Complete)?;
sdk.screen_leave("archive.search")?;
sdk.checkpoint()?;
```

Call `lifecycle` when a window's visibility or focus changes, `interaction` when eligible app input occurs, and `suspend(true/false)` for suspension/resumption. Supply a stable semantic window ID. Call `checkpoint` periodically to emit lifecycle totals. Checkpoints contain cumulative totals for the current session, not intervals. Queries should take the latest checkpoint or compute ordered deltas within a session; summing successive checkpoints double-counts time. Framework-specific automatic detection is not installed.

The native engine also provides a local cumulative aggregate snapshot:

```rust
let totals: telemetry::Aggregates = sdk.aggregates();
for (name, counter) in totals.counters {
    println!("{name}: {} samples, sum {}", counter.count, counter.sum);
}
for (name, span) in totals.spans {
    println!("{name}: {} samples, total {} ms", span.count, span.sum_ms);
}
```

`counters` contains each metric's `count` and `sum`. `spans` contains `count`, `sum_ms`, `min_ms`, `max_ms` and nine histogram `buckets`. Their upper-inclusive millisecond boundaries are `1, 5, 10, 50, 100, 500, 1000, 5000`, followed by a final bucket above 5000 (`+Inf`). Each sample increments exactly one bucket; these counts are not cumulative histogram counts. The Rust constant is `SPAN_BUCKET_BOUNDS_MS`.

Each aggregate map is bounded to 256 names. Aggregates are local memory snapshots, separate from queued occurrence events, and are not reset by a flush. Withdrawing `usage_aggregate` clears counter aggregates; withdrawing `performance` clears span aggregates. The shared native command `{"op":"aggregates"}` returns the same snapshot. The browser subset does not expose this native aggregate API.

SQLite persistence is an explicit configuration choice:

```rust
let mut config = telemetry::Config::new("example.desktop", "1.0.0");
config.sqlite_path = Some("/private/application-data/telemetry.db".into());
let sdk = telemetry::init(config)?;
```

The parent directory must exist and be writable. Treat this file as sensitive; the SDK does not encrypt it. Initialization and collection against SQLite perform synchronous local I/O. Rust, Python and Go hosts should use an application worker when UI responsiveness matters; the absence of network work on a collection call does not make disk operations asynchronous.

The optional tracing layer can be composed with an application's existing subscriber. Only error events with a valid explicit semantic code are exported:

```rust
use tracing_subscriber::prelude::*;
let subscriber = tracing_subscriber::registry()
    .with(telemetry::TelemetryLayer::new(sdk.clone()));
tracing::subscriber::with_default(subscriber, || {
    tracing::error!(telemetry_code = "import.failed", "local log message");
});
```

The application must depend on `tracing` and `tracing-subscriber` for this example. The bridge does not install a global subscriber or forward the log message.

## Python

For local development with the host's Python:

```sh
cargo build -p telemetry-python --features extension-module
python3 packages/python/build_native.py
PYTHONPATH=packages/python python3 examples/python/main.py
```

For an installable source package, create an application virtual environment and install the local package with pip; its build backend uses maturin and requires Rust:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install ./packages/python
```

```python
import telemetry

sdk = telemetry.init({"app_id": "example.python", "app_version": "1.0.0"})
sdk.set_consent("usage_aggregate", True)
sdk.action("search.submit", result="success")
sdk.increment("documents.imported", 3)
sdk.set_consent("performance", True)
with sdk.span("document.ocr"):
    pass
```

The Python API is synchronous. Native work releases the GIL, but SQLite I/O, consent changes, `sdk.flush(timeout_ms=5000)` and `sdk.shutdown(timeout_ms=5000)` can still block the calling thread; run them outside a GUI event thread. Use a real endpoint to flush queued data, or withdraw the relevant permissions to purge a local demonstration before shutdown. `TelemetryHandler(sdk)` is an opt-in `logging.Handler` for ERROR-or-higher entries and exports the fixed error code `python.logging` without formatted text or tracebacks.

## C and C++

```sh
cargo build -p telemetry-ffi --release
cc -std=c11 -Wall -Wextra -pthread -Iinclude examples/c/smoke.c -Ltarget/release -ltelemetry_ffi \
  -Wl,-rpath,"$PWD/target/release" -o /tmp/telemetry-c-smoke
/tmp/telemetry-c-smoke
```

This linker command targets Linux. Use the matching dynamic-library conventions on other platforms. The header documents pointer/length requirements, status codes, concurrency and buffer ownership. Release every non-null response buffer exactly once with `telemetry_v1_buffer_free`, including responses returned with an error status. Inputs must remain valid for each call.

Commands use a JSON `op`, for example:

```json
{"op":"action","id":"search.submit","result":"success"}
```

Send a `set_consent` command first to enable its category. A `shutdown` command reports final export failures; destroying a handle invalidates it even if its best-effort export cannot complete.

## Node.js and TypeScript

```sh
cargo build -p telemetry-node
node packages/js/build-native.mjs
node examples/js/main.mjs
```

Use the ESM package directly during local development:

```javascript
import { createTelemetry } from './packages/js/index.mjs';

const sdk = await createTelemetry({ app_id: 'example.node', app_version: '1.0.0' });
await sdk.setConsent('usage_aggregate', true);
await sdk.action('search.submit', { result: 'success' });
await sdk.increment('documents.imported', 3);
```

The package includes TypeScript declarations. Initialization and every SDK method return promises and dispatch native work through asynchronous N-API tasks, including collection, SQLite work, diagnostics and capability queries. Await each call to preserve application ordering. For example, use `await sdk.diagnostics()` and `await sdk.capabilities()`.

Enable `performance` before starting a span. `const finish = await sdk.span(name)` returns an idempotent asynchronous finish function; call `await finish()` after the measured operation. `flush(timeoutMs)` and `shutdown(timeoutMs)` are also asynchronous.

## Go

The local module is `telemetry.local/sdk`. The example module uses a local `replace` directive. A matching native library and cgo are required:

```sh
cargo build --release -p telemetry-ffi
(cd packages/go && CGO_ENABLED=1 LD_LIBRARY_PATH=../../target/release go test ./...)
(cd examples/go && CGO_ENABLED=1 LD_LIBRARY_PATH=../../target/release go run .)
```

These loader commands target Linux. On macOS use `DYLD_LIBRARY_PATH`; on Windows put the matching DLL on `PATH`. The [package instructions](../packages/go/README.md) explain include/linker paths when copying the module outside this workspace.

```go
sdk, err := telemetry.New(telemetry.Config{AppID: "example.go", AppVersion: "1.0"})
if err != nil { return err }
if err := sdk.SetConsent(telemetry.UsageAggregate, true); err != nil { return err }
if err := sdk.Action("search.submit", telemetry.Success); err != nil { return err }
```

The Go API is synchronous. SQLite collection can perform local disk I/O; use an application worker goroutine when appropriate. `Flush(ctx)` and `Close(ctx)` block the calling goroutine and use the context's deadline. Cancellation without a deadline cannot interrupt an active FFI call. `CGO_ENABLED=0` permits compilation, but creating an SDK returns `ErrCGODisabled`; no pure-Go telemetry fallback is provided.

## Dart and native Flutter

The package is named `telemetry_native`. Add `packages/flutter` as a path dependency in the consuming app. All calls are asynchronous; native FFI work runs in a dedicated isolate.

```sh
cargo build --release -p telemetry-ffi
dart analyze packages/flutter/lib packages/flutter/example
dart run packages/flutter/example/smoke.dart "$PWD/target/release/libtelemetry_ffi.so"
```

The sample library filename targets Linux. Pass the matching loadable library path for your platform:

```dart
import 'package:telemetry_native/telemetry.dart';

final sdk = await Telemetry.initialize(
  {'app_id': 'example.dart', 'app_version': '1.0'},
  libraryPath: '/absolute/path/libtelemetry_ffi.so',
);
await sdk.setConsent('usage_aggregate', true);
await sdk.action('search.submit', result: ActionResult.success);
await sdk.increment('documents.imported', 3);
```

The host supplies Flutter lifecycle, window and navigation callbacks. Automatic widgets/routes/plugins are not bundled. [Package instructions](../packages/flutter/README.md) cover native-library packaging and the unverified Android/iOS limitations. This package does not support Flutter Web; use the browser adapter there.

## Browser JavaScript and WASM

Install the `wasm32-unknown-unknown` Rust target and a `wasm-bindgen` CLI version matching the version resolved in `Cargo.lock`. Build the browser artifacts:

```sh
rustup target add wasm32-unknown-unknown
bash packages/js/build-wasm.sh
```

Serve the JavaScript package and its generated `wasm/` directory through your application's web server, with the WASM MIME type configured correctly. In a browser module:

```javascript
import { createBrowserTelemetry } from './packages/js/browser.mjs';

const sdk = await createBrowserTelemetry({
  app_id: 'example.browser',
  app_version: '1.0.0',
  endpoint: 'https://collector.example.com',
});
sdk.setConsent('usage_aggregate', true);
sdk.action('search.submit', { result: 'success' });
await sdk.flush(5000);
```

The collector or its HTTPS proxy must allow the application's origin and requested headers through CORS. OTLP uses `/v1/logs`, `/v1/metrics` and `/v1/traces`; only signals present in the queue are sent. The adapter uses `fetch` without credentials or referrer and rejects redirects. Browser consent changes are synchronous; export is asynchronous and explicit. There is no periodic background upload, IndexedDB persistence, page-exit delivery guarantee, or automatic Page Visibility/focus instrumentation. Session telemetry, screens, workflows and lifecycle collection are unsupported and must not be enabled.

After building WASM, the test below exercises the actual WASM engine in Node with a mocked HTTPS `fetch`. It does not constitute a browser or DOM test:

```sh
node packages/js/test/browser-smoke.mjs
```

## Export and delivery

The default exporter uses standard OTLP/HTTP JSON requests and responses, including partial-success handling. Successfully completed signal requests are removed independently, so a failed trace request does not replay an already accepted log request. Definitive OTLP rejections are reported and consumed without retry. Retryable failures retain the affected events subject to queue limits and TTL. See [delivery semantics](opentelemetry.md#delivery-and-retries).

Existing custom receivers can select `export_protocol: "legacy_json"` and retain the full `https://…/v1/telemetry/batches` endpoint. That mode sends `{"schema_version":1,"events":[...]}` and uses its original whole-batch acknowledgment behavior; it is not OTLP.

`flush` is a delivery attempt with a deadline, not proof of durable database storage. A successful acknowledgment means the configured collector or transport accepted the records. Application shutdown, browser exit and mobile suspension cannot guarantee delivery.
