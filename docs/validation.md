# Validation — 2026-11-16

Validated on Linux x86_64. The SDK is source-built and unpublished. This evidence applies to the initial client implementation; the original design's backend and production milestones remain outside it.

| Check | Command | Result |
|---|---|---|
| Rust workspace | `bash scripts/check.sh` | Passed: generated-schema drift, Rustfmt, all-feature workspace tests, memory-only facade tests and all-target/all-feature Clippy with warnings denied. |
| Stable compiler | `cargo +stable check --workspace --all-features --locked --offline` | Passed with Rust 1.96.0. Declared minimum 1.89 was not independently tested. |
| External Rust application | `cargo +stable run --offline --no-default-features` from a separate temporary consumer project | Passed against the facade by absolute path, outside this workspace; exercised actions, counters, spans, screens, workflows, aggregation and shutdown. |
| Native language bindings | `bash scripts/check-bindings.sh` | Passed against the release C library and debug Python/Node extensions: C ownership/concurrency smoke, three shared C-ABI conformance tests, Python and Node smoke, Go race/non-cgo tests, Dart analysis and isolate/FFI smoke. |
| Browser build | `PATH=/tmp/telemetry-wasm-tools/bin:$PATH bash packages/js/build-wasm.sh` | WASM release compilation and JavaScript glue generation passed with matching wasm-bindgen 0.2.129. |
| WASM behavior | `node packages/js/test/browser-smoke.mjs` | Passed using actual generated WASM under Node 22.21.0 and mocked HTTPS fetch. This is not a DOM/browser test. |
| TypeScript declarations | `tsc --noEmit --strict --lib ES2022,DOM --module nodenext --moduleResolution nodenext packages/js/index.d.ts packages/js/browser.d.ts` | Passed using an existing TypeScript 5.5.3 installation. No new npm dependency was added. |
| Schema package | `cargo package -p telemetry-schema --offline --allow-dirty` | Passed, including compilation of the extracted crate. Other packages remain unpublished source distributions. |

The Rust workspace run passed 60 unit/integration tests and one documentation compilation test. The memory-only facade run separately passed five integration tests and its documentation test. The primary test compiler was Rust 1.102.0-nightly; the stable check above was additional verification.

Native script environment on this host:

```sh
GOCACHE=/tmp/telemetry-go-cache \
TELEMETRY_GO=/usr/local/go/bin/go \
TELEMETRY_DART=/opt/flutter/flutter/bin/cache/dart-sdk/bin/dart \
bash scripts/check-bindings.sh
```

Go was 1.26.3, Dart 3.10.4 and Python 3.12.3. Local socket tests required sandbox escalation: unrestricted runs passed after restricted runs failed with `Operation not permitted` while binding loopback sockets. No test was skipped to obtain the passing results.

Reviews found and verified fixes for persisted diagnostic session linkage, browser configuration mutation during initialization, independent browser flush deadlines, Go close deadlines behind active uploads, Python cleanup masking application exceptions, and collection after JavaScript shutdown starts. These checks do not establish a production security certification.

Not established: other operating systems or architectures; Android/iOS application packaging; real browser/DOM behavior; Python wheel installation; signed or published releases; production gateway/database/dashboard/MCP E2E; native crash recovery; encrypted persistence; browser IndexedDB. See [the support matrix](support-matrix.md).
