#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# The native SDK targets the host. Configure normal cross-compilation tooling
# separately for other systems; these checks execute the resulting host libraries.
cargo build -p telemetry-ffi --release --locked
cargo build -p telemetry-python --features extension-module --locked
cargo build -p telemetry-node --locked
python3 packages/python/build_native.py
node packages/js/build-native.mjs
PYTHONPATH=packages/python python3 packages/python/tests/smoke.py
node packages/js/test/smoke.mjs

case "$(uname -s)" in
  Linux)
    telemetry_library="$PWD/target/release/libtelemetry_ffi.so"
    export LD_LIBRARY_PATH="$PWD/target/release${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    ;;
  Darwin)
    telemetry_library="$PWD/target/release/libtelemetry_ffi.dylib"
    export DYLD_LIBRARY_PATH="$PWD/target/release${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
    ;;
  *) printf '%s\n' 'Run the documented native checks for your platform.' >&2; exit 1 ;;
esac
cc -std=c11 -Wall -Wextra -Werror -pthread -Iinclude examples/c/smoke.c \
  -Ltarget/release -ltelemetry_ffi -o target/telemetry-c-smoke
target/telemetry-c-smoke
python3 tests/native_conformance.py "$telemetry_library"

telemetry_go="${TELEMETRY_GO:-go}"
telemetry_dart="${TELEMETRY_DART:-dart}"
(cd packages/go && CGO_ENABLED=1 "$telemetry_go" test -race -count=1 ./...)
(cd packages/go && CGO_ENABLED=0 "$telemetry_go" test ./...)
"$telemetry_dart" --suppress-analytics analyze packages/flutter/lib packages/flutter/example
"$telemetry_dart" --suppress-analytics run packages/flutter/example/smoke.dart "$telemetry_library"
