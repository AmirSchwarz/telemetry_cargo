#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
bindgen="${WASM_BINDGEN_BIN:-wasm-bindgen}"
if ! command -v "$bindgen" >/dev/null || [ "$("$bindgen" --version)" != "wasm-bindgen 0.2.129" ]; then
  printf '%s\n' 'Build requires wasm-bindgen-cli 0.2.129, matching Cargo.toml.' 'Install it into a local tool directory and set WASM_BINDGEN_BIN=/path/to/bin/wasm-bindgen.' >&2
  exit 2
fi
cargo build -p telemetry-wasm --target wasm32-unknown-unknown --release
"$bindgen" --target web --out-dir packages/js/wasm target/wasm32-unknown-unknown/release/telemetry_wasm.wasm
