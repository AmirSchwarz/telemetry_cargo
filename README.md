# Telemetry SDK

A Rust telemetry engine with language bindings for applications. Each SDK instance has its own configuration, consent, schema registry and bounded queue. All optional telemetry is disabled by default.

This repository implements the initial SDK from [the design](04_rust_multilanguage_library_design.md). It is source-only and unpublished. The proposed backend, analytics dashboards, release signing and native crash reporting are separate work; see the [support matrix](docs/support-matrix.md).

## Use from Rust

Add the local crate to an application's `Cargo.toml`:

```toml
[dependencies]
telemetry = { package = "telemetry-rust", path = "/absolute/path/to/telemetrics/crates/telemetry-rust" }
```

```rust
use std::time::Duration;
use telemetry::{ActionResult, Category, Config};

fn main() -> Result<(), telemetry::Error> {
    let mut config = Config::new("example.desktop", "1.0.0");
    config.endpoint = Some("https://telemetry.example.com/v1/telemetry/batches".into());
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

The endpoint must be the full HTTPS batch URL. Omitting it keeps collection local; flushing a nonempty queue without a transport returns an error and preserves the queued events.

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

The canonical built-in registry is [schemas/registry-v1.json](schemas/registry-v1.json). The versioned C ABI is declared in [include/telemetry.h](include/telemetry.h); bindings send commands to the same Rust dispatcher. There is no client dependency on ClickStack or Aptabase.

Licensed under [Apache License 2.0](LICENSE). Package namespaces and signing infrastructure must be chosen before package publication. Cargo packages currently set `publish = false`; pushing this source repository does not publish crates, wheels or npm packages. Third-party dependencies retain their own licenses.
