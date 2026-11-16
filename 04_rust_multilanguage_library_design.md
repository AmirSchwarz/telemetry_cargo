# Telemetry SDK — Rust Core and Multi-Language Library Design

**Status:** Proposed architecture · **Version:** 1.0 · **Target:** Proprietary desktop, mobile, server and browser applications

## 1. Executive decision

Build one Rust telemetry engine with stable schemas, privacy enforcement, local aggregation, bounded offline queues and transport. Publish ergonomic language-specific packages for Rust, Python, Go, JavaScript/TypeScript (Node and browser), and Flutter/Dart. Native bindings reuse the Rust engine where practical; the browser uses a Rust-to-WASM subset plus browser-specific TypeScript integrations. Go uses a cgo/C-ABI wrapper with a pure-Go fallback where cross-compilation or static builds make cgo unsuitable. No application must require ClickStack or Aptabase libraries.

**Non-goals:** keyboard logging, cross-application tracking, screenshots, automatic screen replay, collecting document contents, replacing standard language logging systems, claiming to measure user attention from focus alone.

## 2. Functional scope

| Domain | Signals | Output |
|---|---|---|
| Errors | handled errors, panics, unhandled exceptions, crash markers | sanitized error occurrence and release grouping |
| Performance | operation spans, startup, latency, UI freezes, dropped frames | histograms, slow-operation samples |
| Resources | app-process CPU, RSS, optional GPU/energy metrics | sampled and aggregated resource measures |
| Lifecycle | start, stop, suspend, resume, focus, blur, visibility, minimize | session summary and duration counters |
| Usage | screen enter/leave, semantic actions, feature counters | screen and action aggregates |
| Workflows | start, steps, complete, cancel, fail, timeout | funnel/completion metrics and reason codes |
| Health | exporter failures, queue depth, event drops | local SDK diagnostics, safely bounded |

Track **running**, **foreground/visible**, **focused**, **interaction-active**, and **idle** time separately. Focus does not imply active engagement. Idle threshold configurable (default proposed 90s); reading/viewing screens may opt into passive-visible metrics. Multiple windows require per-window state and de-duplicated app-level time. Use monotonic time for durations; wall clock only for event timestamps.

## 3. Layered design

```
Application / UI framework
  ├─ Rust facade / tracing adapter
  ├─ Python package / logging adapter / PyO3
  ├─ Go package / slog adapter / cgo C ABI (fallback: pure Go protocol)
  ├─ Node.js package / N-API native addon
  ├─ Browser TypeScript adapter / WASM subset + Web APIs
  └─ Dart/Flutter package / dart:ffi + lifecycle/widgets
                  ↓
       Rust engine and stable C ABI
  ├─ typed event model + schema registry
  ├─ privacy gate + redaction + data budgets
  ├─ lifecycle/session state machine
  ├─ counters, histograms, time aggregation
  ├─ bounded queue + optional encrypted persistence
  ├─ batch transport, retry, backoff
  └─ metrics/logging adapters
                  ↓
          HTTPS ingestion gateway
                  ↓
      OTLP/ClickStack + ClickHouse analytics
                  ↓
        dashboards and read-only MCP
```

Native platform event detection lives in the adapter (toolkit APIs) rather than assuming a cross-platform Rust crate can infer foreground state. The engine consumes normalized events. The browser implementation must use IndexedDB or other browser-supported storage instead of SQLite, and browser restrictions may prevent reliable exit uploads. On iOS/Android, OS suspension similarly limits final flush.

## 4. Repository and crate layout

```
telemetry/
  crates/
    telemetry-schema/          # event types, validation, versioning
    telemetry-core/            # engine, aggregation, session state
    telemetry-privacy/         # allowlists, redaction, consent gates
    telemetry-queue/           # bounded in-memory + SQLite storage
    telemetry-transport/       # HTTP batches, OTLP mapping
    telemetry-platform/        # process metrics and OS abstractions
    telemetry-rust/            # public Rust facade + tracing integration
    telemetry-ffi/             # stable C ABI + header generation
    telemetry-python/          # PyO3 module and logging integration
    telemetry-node/            # napi-rs addon
    telemetry-wasm/            # wasm-bindgen subset
  packages/
    python/                    # Python packaging, stubs, wheels
    go/                        # cgo wrapper and fallback
    js/                        # Node and browser TypeScript entrypoints
    flutter/                   # Dart FFI, platform lifecycle plugins
  schemas/                     # versioned JSON schema + generated types
  examples/                    # one app per target ecosystem
  tests/                       # conformance, privacy, stress, interop
  docs/                        # API, privacy, packaging, deployment
```

Use Cargo workspace and feature flags to avoid bundling unused metrics, crash hooks, and SQLite. Generate language models from a canonical schema; do not independently hand-maintain event semantics in five languages.

## 5. Public API

Minimum stable concepts: `initialize(config)`, `shutdown(timeout)`, `set_consent(category, enabled)`, `capture_error(error, context)`, `event(name, properties)`, `increment(metric, amount)`, `screen_enter(id)`, `screen_leave(id)`, `record_action(id, result)`, `workflow_start(name)`, `workflow_step(id, step)`, `workflow_end(id, outcome)`, `span(name)`, `flush(timeout)`, `diagnostics()`.

Rust sketch:

```rust
let sdk = telemetry::init(config)?;
sdk.screen_enter("archive.search");
sdk.action("search.submit", ActionResult::Success);
sdk.increment("documents.imported", 3);
let workflow = sdk.workflow("document.import");
workflow.step("files_selected");
workflow.complete();
```

Python sketch:

```python
sdk = telemetry.init(config)
sdk.screen_enter("archive.search")
sdk.action("search.submit", result="success")
with sdk.span("document.ocr"):
    process_document()
```

Go sketch:

```go
sdk, err := telemetry.New(cfg)
if err != nil { return err }
defer sdk.Close(ctx)
sdk.Action("search.submit", telemetry.Success)
sdk.Increment("documents.imported", 3)
```

TypeScript sketch:

```ts
const sdk = await createTelemetry(config);
sdk.screenEnter('archive.search');
sdk.action('search.submit', { result: 'success' });
sdk.increment('documents.imported', 3);
```

Dart sketch:

```dart
final sdk = await Telemetry.initialize(config);
sdk.screenEnter('archive.search');
sdk.action('search.submit', result: ActionResult.success);
sdk.increment('documents.imported', 3);
```

Examples are **proposed interfaces**, not released APIs. All language bindings must have matching semantics for privacy categories, timestamps, queue behavior, failures and event limits.

## 6. Language interoperability strategy

| Target | Primary path | Notes |
|---|---|---|
| Rust | direct crate | `tracing` subscriber/layer and `log` bridge, optional Tokio integration |
| Python | PyO3/maturin | CPython wheels; Python logging/exception hooks; no interpreter work in exporter threads |
| Go | cgo to C ABI | package prebuilt native libraries by OS/arch; fallback pure-Go collector/HTTP client if cgo unavailable; document CGO_ENABLED=1 constraint |
| Node.js | napi-rs | native binaries per platform/arch; async, no event-loop blocking |
| Browser JS/TS | wasm-bindgen + TS | no native FFI, no OS process metrics; use Page Visibility, focus/blur, Performance API, IndexedDB, sendBeacon where appropriate |
| Flutter/Dart | dart:ffi to C ABI | Android/iOS/macOS/Windows/Linux binaries; Flutter plugin for lifecycle, route and widget semantics; web target uses browser JS/WASM adapter |

**ABI rules:** opaque handles, explicit ownership, pointer+length strings, status/error codes, panic containment, no Rust layout across ABI, thread-safety documented, stable major-version symbols. Build bindings for supported triples and sign/release artifacts. Avoid making Rust FFI mandatory on platforms where it is unworkable.

## 7. Event schema and safety

Envelope fields: `schema_version`, `event_id` (random), `app_id`, `app_version`, `platform`, `sdk_version`, `category`, `event_name`, `occurred_at`, `properties` (allowlisted), optional ephemeral `session_id`, optional `workflow_id`. Do not include raw device IDs, IPs, usernames, file paths, document contents, query text, keyboard events, or full URLs by default. Limit property keys, values, event size, cardinality, and queue volume. Event names come from a versioned registry with allowed fields and privacy classifications.

Categories: `essential_operations` (only after documented necessity/legal review), `diagnostics`, `performance`, `usage_aggregate`, `usage_session`, `extended_support`. The SDK applies permissions **before collection, persistence, and upload**; all optional remote telemetry defaults off. Consent changes must purge disallowed queued data; server must support deletion requests for identifiable data. Aggregates are not automatically anonymous, particularly when small or granular.

## 8. Lifecycle and focus state model

Use orthogonal flags: `process_running`, `app_foreground`, `window_visible`, `window_focused`, `recent_input`, `system_suspended`. Maintain a monotonic-clock accumulator and update on each transition and periodic checkpoint. `interaction_active` requires eligible foreground/focus and recent input; do not record actual input content. `foreground_visible` may be meaningful during reading even without input. Track per-screen dwell, excluding or separately accounting for idle periods. Restore partial sessions after abnormal termination; mark `exit_reason=unknown` unless a crash is confirmed. Keep last-screen and workflow data coarse enough to avoid identifying sensitive documents.

## 9. Storage and transmission

Default memory ring buffer with configurable disk persistence. Native persistence: SQLite WAL with bounded size, TTL, per-category quotas and crash-safe transactions. Encryption-at-rest where available and justified, with explicit key lifecycle; never claim SQLite itself provides encryption. Browser: IndexedDB queue with browser quota/eviction behavior. Batch and compress requests, back off exponentially with jitter, obey Retry-After, deduplicate by event ID on server, bound retry lifetime, and support manual flush. Never block UI/main thread; no network requests inside fatal signal handlers. For fatal native crashes, use a minimal marker and capture richer context on next start; robust native minidumps require platform-specific tooling and separate privacy controls.

## 10. Resource and UI instrumentation

Use app-process CPU and resident memory rather than whole-device telemetry. Sample at configurable intervals (e.g. 30–60s while active); publish aggregate min/max/mean and percentiles where meaningful. GPU, battery, energy and frame timings are optional platform features with explicit capability flags. Integrate UI focus/navigation with framework adapters; for Makepad, Flutter and web, require stable semantic widget IDs and route names. Never infer app-level focus by inspecting other apps or using global key hooks.

## 11. Backend contract

Gateway endpoints: `POST /v1/telemetry/batches`, `GET /health/live`, `GET /health/ready`, optional `POST /v1/telemetry/config`. HTTPS only. Use public per-app ingestion tokens (not admin secrets), payload validation, replay protection, rate limits, schema compatibility, IP log minimization and quotas. The gateway maps diagnostics to OTLP/ClickStack and analytics to ClickHouse tables; Aptabase is an optional later sink, not a client dependency. Use restricted read-only SQL views and a separate MCP service for LLM agents; no raw user payloads or unrestricted database access.

## 12. Security and privacy requirements

Threat model: malicious client payloads, leaked public ingestion tokens, excessive telemetry, PII in exceptions, local queue disclosure, insecure FFI boundaries, prompt injection via log text, accidental agent exposure. Controls: validation, authentication and throttling, size/cardinality budgets, structured redaction, access separation, audit logs, retention and deletion, encrypted transport, dependency scanning, signed releases, and untrusted-data handling for AI agents. German/EU deployment requires GDPR and applicable TDDDG/ePrivacy assessment; technical controls are not a legal basis by themselves. Offer users clear notice and controls, with separate permissions for optional categories.

## 13. Testing and conformance

- Golden event fixtures for all five languages, schema compatibility and time arithmetic.
- Lifecycle state tests: rapid focus switches, minimize, suspend/resume, multiple windows, idle timeout, reading mode, abrupt termination.
- Queue tests: offline days, disk full, corruption, duplicate sends, rate limits, consent withdrawal, shutdown deadlines.
- FFI tests: memory ownership, null/UTF-8 boundaries, concurrency, panic containment, architecture builds.
- Privacy tests: synthetic secrets in exception messages/paths/URLs, field allowlists, queue purge, forbidden event rejection.
- Performance tests: no UI-thread blocking; benchmark CPU, memory, binary size, event throughput and battery impact per platform.
- Server E2E: event emitted → gateway accepted → database stored → aggregate dashboard → read-only MCP query.

## 14. Phased delivery

**Phase 0 — contracts:** schema registry, privacy policy model, platform support matrix, performance budgets, sample apps.

**Phase 1 — Rust MVP:** event API, lifecycle state machine, aggregation, redaction, in-memory and SQLite queue, HTTPS exporter, `tracing` integration.

**Phase 2 — server E2E:** minimal gateway, ClickHouse tables and retention, dashboard queries, synthetic telemetry, rate limiting and secure deploy.

**Phase 3 — Python + Node:** PyO3 and napi-rs packages, exception/log adapters, integration tests and wheels/binaries.

**Phase 4 — Go + Flutter:** C ABI and cgo, Dart FFI plugin, OS lifecycle bridges, binary packaging, Android/iOS smoke tests.

**Phase 5 — browser:** WASM subset + TypeScript browser adapter, IndexedDB, visibility/route tracking, compatibility tests.

**Phase 6 — production hardening:** crash integration, per-platform performance collectors, dashboards, read-only MCP, security/privacy review, releases and upgrade compatibility.

## 15. Definition of done

A Rust, Python, Go, Node, browser and Flutter sample application can each: initialize the SDK; emit a typed action and error; record screen time and focus transitions where supported; enforce disabled optional telemetry; buffer offline; upload to the same gateway; and appear in equivalent server queries. No adapter silently drops unsupported capabilities: it exposes capability discovery and documented fallback behavior. Crash recovery and consent-withdrawal tests pass; supported binaries and licensing are documented; no paid SaaS is required.

## 16. Open decisions

1. Supported minimum OS/runtime versions and target CPU architectures.
2. Whether to ship pure-Go fallback in v1 or mark cgo mandatory initially.
3. Whether Flutter Web must be supported in v1 or after native Flutter.
4. Native crash dump strategy and support-upload consent flow.
5. Exact retention, legal basis and consent UX by application and distribution market.
6. SDK naming, package namespaces and release/signing infrastructure.
