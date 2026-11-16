# Telemetry transport

Without features, this crate supplies the shared transport interface and a
network-free OTLP JSON encoder. Enable `http` for the blocking, Rustls-backed
`OtlpHttpTransport` and the explicit legacy `HttpTransport`. Network calls belong
on an exporter worker; the core owns queue limits, consent and retry scheduling.

`OtlpHttpTransport` targets an existing OpenTelemetry Collector or ClickStack
collector. Its endpoint is a base URL; requests use `/v1/logs`, `/v1/metrics` and
`/v1/traces`. It shares `otlp_requests` with the browser WASM adapter, so native
and browser signal encodings have the same semantics. See the repository's
[OpenTelemetry guide](../../docs/opentelemetry.md) for configuration and mapping.

HTTPS is required by default. The explicit `allow_insecure_loopback` option
permits HTTP only for literal `127.0.0.1` or `::1`. URL credentials, query
strings, fragments, redirects and proxy environment configuration are rejected
or disabled. Custom OTLP headers are bounded; transport/body/cookie/host headers
cannot be overridden. `ingestion_token` means Bearer authentication; a raw
ClickStack key belongs in `otlp_headers["authorization"]`. Combining those
authorization forms is an error. Credentials and raw response text are omitted
from transport errors and Debug output.

The OTLP request budget is 4 MiB across one send attempt. Signal requests share
the caller's timeout, capped at 30 seconds. Responses are limited to 64 KiB,
including after decompression. Only HTTP 200 with valid OTLP JSON is accepted.
Partial-success rejection and definitive nonretryable responses consume the
affected records and report loss without replay. HTTP 429, 502, 503 and 504,
plus transient connection/timeout failures, are retryable. `Retry-After`
supports delta seconds and IMF-fixdate.

`Transport::send_report` returns independently consumed event IDs, rejected
count and an optional error. The default method wraps the original `send`
method with whole-batch behavior, preserving existing custom implementations.
The core validates reports and removes completed signals even if another signal
failed. Direct callers of `send` receive an error for a partial failure and
should use `send_report` when they need per-signal delivery state.

The legacy `HttpTransport` sends `{ "schema_version": 1, "events": [...] }` to
the exact HTTPS `/v1/telemetry/batches` URL. Its whole-batch 2xx acknowledgment
and original retry policy remain unchanged. Core applications select it with
`export_protocol: "legacy_json"`. It cannot target an OTLP receiver.

Request compression, gRPC/protobuf transport, full OpenTelemetry SDK APIs and
distributed trace-context propagation are not implemented. The exporter does
not provide backend storage, retention, deletion or deployment.
