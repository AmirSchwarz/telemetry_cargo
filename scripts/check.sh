#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
python3 schemas/generate.py --check
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
cargo test -p telemetry-rust --no-default-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
