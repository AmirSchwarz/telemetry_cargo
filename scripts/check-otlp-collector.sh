#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# This is a disposable local interoperability check, not a deployment command.
telemetry_collector_image="${TELEMETRY_COLLECTOR_IMAGE:-otel/opentelemetry-collector-contrib:0.165.0}"
# Keep bind mounts in the checkout: Docker Desktop may not share host /tmp.
# target/ is ignored and retains the synthetic result for inspection.
mkdir -p target/otlp-collector
telemetry_output="$(mktemp -d "$PWD/target/otlp-collector/run.XXXXXXXX")"
telemetry_container=""

cleanup() {
  if [[ -n "$telemetry_container" ]]; then
    docker stop --time 5 "$telemetry_container" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

cargo build -p telemetry-ffi --locked
case "$(uname -s)" in
  Linux) telemetry_library="$PWD/target/debug/libtelemetry_ffi.so" ;;
  Darwin) telemetry_library="$PWD/target/debug/libtelemetry_ffi.dylib" ;;
  *) printf '%s\n' 'Run the collector conformance test with your platform library path.' >&2; exit 1 ;;
esac

telemetry_container="$(docker run --detach --rm \
  --user "$(id -u):$(id -g)" \
  --publish 127.0.0.1::4318 \
  --mount "type=bind,src=$PWD/examples/opentelemetry/collector.yaml,dst=/etc/otelcol-contrib/config.yaml,readonly" \
  --mount "type=bind,src=$telemetry_output,dst=/output" \
  "$telemetry_collector_image" --config=/etc/otelcol-contrib/config.yaml)"
telemetry_address="$(docker port "$telemetry_container" 4318/tcp)"

if ! python3 tests/otlp_collector_conformance.py \
  --library "$telemetry_library" \
  --endpoint "http://$telemetry_address" \
  --output "$telemetry_output/telemetry.json"; then
  docker logs --tail 30 "$telemetry_container" >&2 || true
  exit 1
fi
printf 'Collector: %s\nSynthetic output: %s\n' "$telemetry_collector_image" "$telemetry_output/telemetry.json"
