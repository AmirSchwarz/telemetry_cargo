# Telemetry SDK

A Rust telemetry engine with language bindings for applications. Each SDK instance has its own configuration, consent, schema registry and bounded queue. All optional telemetry is disabled by default.

The default exporter sends standard **OTLP/HTTP JSON** to an existing OpenTelemetry Collector or ClickStack collector. You configure the collector's base URL; no custom telemetry API is required. The SDK remains source-built and unpublished. See [OpenTelemetry and ClickStack setup](docs/opentelemetry.md) and the [support matrix](docs/support-matrix.md).

## Use from Rust

Add the crate from GitHub to an application's `Cargo.toml`:

```toml
[dependencies]
telemetry = { package = "telemetry-rust", git = "https://github.com/AmirSchwarz/telemetry_cargo", branch = "main" }
```

Or use a local checkout:

```toml
[dependencies]
telemetry = { package = "telemetry-rust", path = "/absolute/path/to/telemetrics/crates/telemetry-rust" }
```

```rust
use std::time::Duration;
use telemetry::{ActionResult, Category, Config};

fn main() -> Result<(), telemetry::Error> {
    let mut config = Config::new("example.desktop", "1.0.0");
    config.endpoint = Some("https://collector.example.com".into());
    config.ingestion_token = std::env::var("TELEMETRY_INGESTION_TOKEN").ok();
    let sdk = telemetry::init(config)?;

    // Enable categories only after the application has obtained permission.
    sdk.set_consent(Category::UsageAggregate, true)?;
    sdk.action("search.submit", ActionResult::Success)?;
    sdk.increment("documents.imported", 3.0)?;

    // Initialize/use SQLite, flush and shutdown outside the UI thread.
    sdk.shutdown(Duration::from_secs(3))?;
    Ok(())
}
```

For OTLP, `endpoint` is the collector base URL. The exporter appends `/v1/logs`, `/v1/metrics` and `/v1/traces` as needed. HTTPS is the default requirement; local plaintext testing requires an explicit loopback opt-in. Omitting the endpoint keeps collection local; flushing a nonempty queue without a transport returns an error and preserves queued events.

Actions, errors, screens, workflows and lifecycle events become OTLP logs; increments become delta metrics; spans become standalone traces. ClickStack ingestion keys use a raw `authorization` header in `otlp_headers`; `ingestion_token` instead sends a Bearer token. See the [configuration examples](docs/opentelemetry.md#clickstack-authentication).

The facade's default features are `http`, `sqlite` and `tracing`. To use memory queues and your own transport without those integrations:

```toml
telemetry = { package = "telemetry-rust", path = "/absolute/path/to/telemetrics/crates/telemetry-rust", default-features = false }
```

## Build and check

The Cargo workspace declares Rust 1.89 or later. Run from the repository root:

```sh
cargo build
cargo run -p telemetry-rust --example basic
cargo test --workspace --all-features --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
bash scripts/check.sh
```

The [check script](scripts/check.sh) also verifies generated schema artifacts and the facade without default features. [Native binding checks](scripts/check-bindings.sh) build and execute C, Python, Node, Go and Dart integration tests; they require those host toolchains. The [validation report](docs/validation.md) records the results and their platform limits. The [usage guide](docs/usage.md) covers each language and browser builds. The [privacy guide](docs/privacy.md) explains collection and withdrawal behavior.

The canonical built-in registry is [schemas/registry-v1.json](schemas/registry-v1.json). The versioned C ABI is declared in [include/telemetry.h](include/telemetry.h); bindings send commands to the same Rust dispatcher. There is no client dependency on ClickStack or Aptabase. This is an OTLP exporter with privacy controls, not a replacement for the full OpenTelemetry SDK or distributed trace-context propagation.

Licensed under [Apache License 2.0](LICENSE). Package namespaces and signing infrastructure must be chosen before package publication. Cargo packages currently set `publish = false`; pushing this source repository does not publish crates, wheels or npm packages. Third-party dependencies retain their own licenses.
