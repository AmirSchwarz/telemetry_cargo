# OpenTelemetry and ClickStack

The SDK sends standard OTLP/HTTP JSON to an existing OpenTelemetry Collector.
ClickStack includes a compatible collector. You supply its base URL and
authentication; you do not need to write the previous custom telemetry API.
The collector and backend must still be running, either self-hosted or supplied
by a service.

The default is `export_protocol: "otlp_http_json"`. OTLP/gRPC, binary protobuf,
auto-instrumentation and distributed context propagation are not implemented.
The SDK's consent, redaction and bounded queues remain in front of export.

## Connect an application

For a collector exposed through HTTPS:

```rust
use std::time::Duration;
use telemetry::{ActionResult, Category, Config};

let mut config = Config::new("example.desktop", "1.0.0");
config.endpoint = Some("https://collector.example.com".into());
// If the collector requires Bearer authentication:
config.ingestion_token = std::env::var("TELEMETRY_INGESTION_TOKEN").ok();
let sdk = telemetry::init(config)?;
sdk.set_consent(Category::UsageAggregate, true)?;
sdk.set_consent(Category::Diagnostics, true)?;
sdk.set_consent(Category::Performance, true)?;
sdk.action("search.submit", ActionResult::Success)?;
sdk.increment("documents.imported", 3.0)?;
sdk.capture_error("import.failed", "This raw message is redacted")?;
sdk.span("document.ocr")?.finish()?;
sdk.shutdown(Duration::from_secs(5))?;
```

Use the collector's **base URL**, not a signal URL or the legacy batch URL:

| Configuration | Requests made by the SDK |
|---|---|
| `https://collector.example.com` | `/v1/logs`, `/v1/metrics`, `/v1/traces` |
| `https://collector.example.com/otlp` | `/otlp/v1/logs`, `/otlp/v1/metrics`, `/otlp/v1/traces` |

Only signals present in the queued batch are requested. Do not append `/v1`,
`/v1/logs`, `/v1/metrics`, `/v1/traces` or `/v1/telemetry/batches` to an OTLP
base endpoint. URLs cannot contain credentials, a query string or a fragment.

The same JSON configuration works through Python, Node, C and Dart. Go exposes
typed configuration fields. For example, Node:

```javascript
const sdk = await createTelemetry({
  app_id: 'example.node',
  app_version: '1.0.0',
  endpoint: 'https://collector.example.com',
  export_protocol: 'otlp_http_json',
});
await sdk.setConsent('usage_aggregate', true);
await sdk.action('search.submit', { result: 'success' });
await sdk.flush(5000);
```

The SDK does not implicitly read `OTEL_EXPORTER_*` or `TELEMETRY_*` environment
variables. Applications choose and assign configuration explicitly. All consent
categories remain disabled until the application enables them.

## ClickStack authentication

Use the OTLP/HTTP endpoint of your ClickStack collector, typically port 4318
when exposed directly. For a secured ClickStack deployment, supply its public
ingestion API key as the **raw authorization header** expected by that
deployment:

```rust
let mut config = telemetry::Config::new("example.desktop", "1.0.0");
config.endpoint = Some("https://collector.example.com".into());
config.otlp_headers.insert(
    "authorization".into(),
    std::env::var("CLICKSTACK_INGESTION_KEY")?,
);
```

Equivalent JSON configuration for the language bindings:

```json
{
  "app_id": "example.desktop",
  "app_version": "1.0.0",
  "endpoint": "https://collector.example.com",
  "otlp_headers": { "authorization": "<public ingestion API key>" }
}
```

`ingestion_token` adds `Authorization: Bearer <token>` instead. Do not set it
together with an `authorization` entry in `otlp_headers`; that conflict is
rejected. A collector requiring Basic or another header scheme can receive
the complete expected value in `otlp_headers`. Never ship ClickHouse database
credentials or an administration API key in an application.

ClickStack editions and deployment methods differ in how collectors are
secured. Use the endpoint and ingestion key for the deployment you run; do not
assume that a database HTTPS endpoint is an OTLP collector endpoint. The
[official ClickStack collector guide](https://clickhouse.com/docs/use-cases/observability/clickstack/ingesting-data/otel-collector)
describes collector deployment and its authorization header.

## Local collector testing

An unencrypted local collector requires explicit opt-in:

```rust
let mut config = telemetry::Config::new("example.local", "1.0.0");
config.endpoint = Some("http://127.0.0.1:4318".into());
config.allow_insecure_loopback = true;
```

The exception permits only literal `127.0.0.1` or `::1`, not arbitrary hosts or
the hostname `localhost`. Leave it false for remote deployments and use HTTPS.
The existing collector must enable its OTLP HTTP receiver and the signal
pipelines needed by the app. Browser use additionally needs collector/proxy
CORS rules for the application's origin, content type and authentication
headers.

The repository includes a local conformance receiver configuration at
[examples/opentelemetry/collector.yaml](../examples/opentelemetry/collector.yaml)
and a runner:

```sh
bash scripts/check-otlp-collector.sh
```

The runner starts a temporary official OpenTelemetry Collector, drives the
compiled C ABI and checks accepted logs, metrics and traces through its file
exporter. This validates collector ingestion; it does not establish ClickHouse
storage or ClickStack UI/dashboard behavior. See the runner for its Docker,
Python and native-build prerequisites.

## Signal mapping

| SDK data | OTLP output |
|---|---|
| Actions, errors, screens, workflows, lifecycle checkpoints and registered custom events | Log records at `/v1/logs` |
| `increment("documents.imported", 3)` | Delta Sum metric `telemetry.documents.imported` at `/v1/metrics` |
| `span("document.ocr")` | Standalone internal span at `/v1/traces` |

Resources include `service.name` from `app_id`, `service.version` from
`app_version`, the SDK name/version and platform. Instrumentation scope is
`telemetry-cargo`. Use stable service IDs and version strings, not user or
device identifiers.

Log bodies contain the semantic event name. Allowed properties use
`telemetry.property.<key>` attributes; category, schema version and random event
ID use `telemetry.*` attributes. Error logs have ERROR severity, and raw error
messages remain `[redacted]`. Session/workflow IDs appear only when permitted
on session-category data.

Metrics preserve signed increments with delta temporality and
`isMonotonic=false`. They are discrete event deltas, not continuously
aggregated OpenTelemetry SDK counters. Optional stream start timestamps are
omitted rather than invented. Metric labels omit event/session/workflow IDs
and arbitrary event properties, avoiding a new series for every event.

Spans use monotonic measured durations and the event's wall-clock timestamp.
Their trace/span IDs derive from the random event ID, so retries keep the same
identity. They are standalone roots: the SDK does not extract/inject W3C trace
context or join a distributed parent trace. Local histogram snapshots returned
by `aggregates()` are not automatically exported as OTLP histograms.

## Delivery and retries

Logs, metrics and traces are separate HTTP requests sharing the send deadline.
The SDK removes each completed signal independently. If logs succeed and
metrics fail transiently, retrying the remaining queue does not resend those
accepted logs.

A valid HTTP 200 OTLP JSON response acknowledges a request. Partial-success
rejection is reported, counted as dropped data and consumed without retry.
Definitive nonretryable HTTP rejection likewise removes the affected records.
HTTP 429, 502, 503 and 504, plus transient connection/timeouts, can retry within
the deadline and queue lifetime. `Retry-After` supports seconds and IMF-fixdate.
Malformed or oversized success responses are errors, not proof of acceptance.
Server error text and credentials are not echoed into SDK diagnostics.

OTLP delivery can still duplicate data when a connection fails after a server
accepted a request but before the SDK received its response. A standard
collector does not promise deduplication by this SDK's event ID. A successful
flush proves collector acceptance, not durable ClickHouse storage or dashboard
visibility.

## Keep an existing custom receiver

For a previously implemented custom batch receiver, select the legacy
protocol explicitly:

```json
{
  "app_id": "example.legacy",
  "app_version": "1.0.0",
  "export_protocol": "legacy_json",
  "endpoint": "https://telemetry.example.com/v1/telemetry/batches"
}
```

That mode retains the original `{ "schema_version": 1, "events": [...] }`
request format and whole-batch acknowledgment. It does not accept OTLP-only
header or insecure-loopback options. Existing configuration using the old URL
must add `export_protocol: "legacy_json"`; the new default is OTLP.

The [OTLP specification](https://opentelemetry.io/docs/specs/otlp/) defines the
wire encoding, endpoints and response/retry semantics. The [Collector
architecture](https://opentelemetry.io/docs/collector/architecture/) describes
receivers, processors and exporters. The SDK does not deploy or administer
those backend services.
