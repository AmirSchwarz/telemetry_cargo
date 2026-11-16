# Telemetry transport

The default build supplies the `Transport` interface without networking. Enable
`http` for `HttpTransport`, a blocking Rustls-backed HTTPS batch client. Call
`send` on an exporter worker. It does not retry on its own; the engine retains
event IDs in its bounded queue and owns retry scheduling, consent and deadlines.

Production endpoints must be full HTTPS URLs ending in exactly
`/v1/telemetry/batches`, without credentials, query strings or fragments.
Redirects and proxy environment configuration are disabled to keep the
destination and credential boundary explicit. `for_loopback_test` is a separate
constructor permitting HTTP only at literal `127.0.0.1` or `::1` addresses for
synthetic tests.

Requests contain JSON `{ "schema_version": 1, "events": [...] }`. Bodies are
limited to 4 MiB during serialization. An attempt is limited to the smaller of
the caller's deadline and 30 seconds. Only 2xx responses acknowledge a whole
batch. Connection failures, 408, 429 and 5xx responses are retryable; other
statuses are permanent failures. `Retry-After` supports delta seconds and
IMF-fixdate; obsolete RFC 850 and asctime date formats are unsupported.

The transport never includes response bodies, endpoint strings or tokens in
its own errors or debug output. Public ingestion tokens are sent as Bearer
tokens and are validated before use. The transport does not provide server
authorization, deduplication, deletion, quotas or storage; these remain gateway
responsibilities. Request compression is not implemented in 0.1.
