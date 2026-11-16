//! Network-free OTLP JSON mapping shared by native and browser exporters.
//!
//! The application engine must apply consent and its custom registry before
//! calling this mapper. Built-in envelopes are checked again here so direct
//! transport use cannot accidentally export an unredacted error message.

use crate::{TransportError, MAX_BATCH_BYTES};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use telemetry_schema::{validate_envelope, Category, Event, Limits, Registry};

const MAX_EVENTS: usize = 1_024;
const NANOS_PER_MILLISECOND: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OtlpSignal {
    Logs,
    Metrics,
    Traces,
}

impl OtlpSignal {
    pub fn path(self) -> &'static str {
        match self {
            Self::Logs => "/v1/logs",
            Self::Metrics => "/v1/metrics",
            Self::Traces => "/v1/traces",
        }
    }

    pub fn rejected_field(self) -> &'static str {
        match self {
            Self::Logs => "rejectedLogRecords",
            Self::Metrics => "rejectedDataPoints",
            Self::Traces => "rejectedSpans",
        }
    }
}

/// A single signal's OTLP request and the queue records it represents.
/// Only `body` is the wire payload; event IDs remain local delivery bookkeeping.
#[derive(Clone, Debug, Serialize)]
pub struct OtlpRequest {
    pub signal: OtlpSignal,
    pub path: String,
    pub event_ids: Vec<String>,
    pub body: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ResourceKey {
    app_id: String,
    app_version: String,
    platform: String,
    sdk_version: String,
}

impl ResourceKey {
    fn from_event(event: &Event) -> Self {
        Self {
            app_id: event.app_id.clone(),
            app_version: event.app_version.clone(),
            platform: event.platform.clone(),
            sdk_version: event.sdk_version.clone(),
        }
    }

    fn resource(&self) -> Value {
        json!({"attributes": [
            attribute("service.name", json!({"stringValue":self.app_id})),
            attribute("service.version", json!({"stringValue":self.app_version})),
            attribute("telemetry.sdk.name", json!({"stringValue":"telemetry-cargo"})),
            attribute("telemetry.sdk.language", json!({"stringValue":"rust"})),
            attribute("telemetry.sdk.version", json!({"stringValue":self.sdk_version})),
            attribute("telemetry.platform", json!({"stringValue":self.platform}))
        ]})
    }

    fn scope(&self) -> Value {
        json!({"name":"telemetry-cargo", "version":self.sdk_version})
    }
}

#[derive(Default)]
struct ResourceRecords {
    logs: Vec<Value>,
    metrics: BTreeMap<String, Vec<Value>>,
    traces: Vec<Value>,
}

/// Map already consent-filtered, registry-validated events to OTLP/HTTP JSON.
/// No thread, clock, platform, or network API is used, including on WASM.
/// Resource identity is shared, but individual event IDs never become metric
/// dimensions. Increment measurements are discrete signed deltas; an unknown
/// continuous stream start time is deliberately omitted rather than invented.
pub fn otlp_requests(events: &[Event]) -> Result<Vec<OtlpRequest>, TransportError> {
    if events.len() > MAX_EVENTS {
        return Err(invalid("OTLP event count exceeds encoding budget"));
    }
    // Respect every valid engine limit while enforcing this transport's own
    // aggregate payload budget independently of the application's queue limits.
    let limits = Limits {
        max_properties: 128,
        max_string_bytes: 4_096,
        max_event_bytes: 1024 * 1024,
    };
    let registry = Registry::default();
    let mut resources = BTreeMap::<ResourceKey, ResourceRecords>::new();
    let mut ids = BTreeMap::<OtlpSignal, Vec<String>>::new();
    let mut seen_ids = BTreeSet::new();
    let mut charged_resources = BTreeSet::new();
    let mut build_budget = Budget(MAX_BATCH_BYTES);
    for event in events {
        validate_envelope(event, &limits).map_err(|_| invalid("invalid OTLP event envelope"))?;
        if registry.definition(&event.event_name).is_some() {
            registry
                .validate(event, &limits)
                .map_err(|_| invalid("OTLP event violates builtin schema"))?;
        }
        if !seen_ids.insert(event.event_id.as_str()) {
            return Err(invalid("duplicate OTLP event ID"));
        }
        let timestamp = event
            .occurred_at
            .checked_mul(NANOS_PER_MILLISECOND)
            .ok_or_else(|| invalid("OTLP timestamp exceeds supported range"))?;
        let (signal, record, metric_name) = match event.event_name.as_str() {
            "span" => (OtlpSignal::Traces, span(event, timestamp)?, None),
            "metric.increment" => {
                let name = required_string(event, "name")?;
                let amount = required_number(event, "amount")?;
                let point = json!({
                    "timeUnixNano":timestamp.to_string(),
                    "asDouble":amount,
                    "attributes":[attribute("telemetry.category",json!({"stringValue":category_name(event.category)}))]
                });
                (
                    OtlpSignal::Metrics,
                    point,
                    Some(format!("telemetry.{name}")),
                )
            }
            _ => (OtlpSignal::Logs, log(event, timestamp)?, None),
        };
        charge(&mut build_budget, &record)?;
        let key = ResourceKey::from_event(event);
        if charged_resources.insert((key.clone(), signal)) {
            charge(&mut build_budget, &key.resource())?;
            charge(&mut build_budget, &key.scope())?;
        }
        let records = resources.entry(key).or_default();
        match signal {
            OtlpSignal::Logs => records.logs.push(record),
            OtlpSignal::Metrics => {
                let name = metric_name.expect("metric name provided for metric record");
                if !records.metrics.contains_key(&name) {
                    charge(&mut build_budget, &name)?;
                }
                records.metrics.entry(name).or_default().push(record);
            }
            OtlpSignal::Traces => records.traces.push(record),
        }
        ids.entry(signal).or_default().push(event.event_id.clone());
    }

    let mut logs = Vec::new();
    let mut metrics = Vec::new();
    let mut traces = Vec::new();
    for (key, records) in resources {
        if !records.logs.is_empty() {
            logs.push(json!({
                "resource":key.resource(),
                "scopeLogs":[{"scope":key.scope(),"logRecords":records.logs}]
            }));
        }
        if !records.metrics.is_empty() {
            let instruments: Vec<Value> = records
                .metrics
                .into_iter()
                .map(|(name, points)| {
                    json!({"name":name,"sum":{
                        "aggregationTemporality":1,
                        "isMonotonic":false,
                        "dataPoints":points
                    }})
                })
                .collect();
            metrics.push(json!({
                "resource":key.resource(),
                "scopeMetrics":[{"scope":key.scope(),"metrics":instruments}]
            }));
        }
        if !records.traces.is_empty() {
            traces.push(json!({
                "resource":key.resource(),
                "scopeSpans":[{"scope":key.scope(),"spans":records.traces}]
            }));
        }
    }
    let mut requests = Vec::new();
    let mut payload_budget = Budget(MAX_BATCH_BYTES);
    for (signal, body) in [
        (OtlpSignal::Logs, json!({"resourceLogs":logs})),
        (OtlpSignal::Metrics, json!({"resourceMetrics":metrics})),
        (OtlpSignal::Traces, json!({"resourceSpans":traces})),
    ] {
        if let Some(event_ids) = ids.remove(&signal) {
            charge(&mut payload_budget, &body)?;
            requests.push(OtlpRequest {
                signal,
                path: signal.path().to_owned(),
                event_ids,
                body,
            });
        }
    }
    Ok(requests)
}

fn log(event: &Event, timestamp: u64) -> Result<Value, TransportError> {
    let is_error = event.event_name == "error";
    let mut attributes = common_attributes(event);
    for (key, value) in &event.properties {
        attributes.push(attribute(
            &format!("telemetry.property.{key}"),
            any_value(value)?,
        ));
    }
    Ok(json!({
        "timeUnixNano":timestamp.to_string(),
        "severityNumber":if is_error {17} else {9},
        "severityText":if is_error {"ERROR"} else {"INFO"},
        "body":{"stringValue":event.event_name},
        "attributes":attributes
    }))
}

fn span(event: &Event, end: u64) -> Result<Value, TransportError> {
    let name = required_string(event, "name")?;
    let duration = required_number(event, "duration_ms")?;
    let duration_nanos = duration * NANOS_PER_MILLISECOND as f64;
    // f64 rounds u64::MAX to 2^64; compare against that exclusive bound before
    // casting so saturating float casts cannot conceal overflow.
    if duration < 0.0
        || !duration_nanos.is_finite()
        || duration_nanos >= 18_446_744_073_709_551_616.0
    {
        return Err(invalid("OTLP span duration exceeds supported range"));
    }
    let trace_id = event.event_id.replace('-', "").to_ascii_lowercase();
    let span_id = &trace_id[16..];
    Ok(json!({
        "traceId":trace_id,
        "spanId":span_id,
        "name":name,
        "kind":1,
        "startTimeUnixNano":end.saturating_sub(duration_nanos as u64).to_string(),
        "endTimeUnixNano":end.to_string(),
        "attributes":common_attributes(event)
    }))
}

fn common_attributes(event: &Event) -> Vec<Value> {
    let mut values = vec![
        attribute("telemetry.event_id", json!({"stringValue":event.event_id})),
        attribute(
            "telemetry.category",
            json!({"stringValue":category_name(event.category)}),
        ),
        attribute(
            "telemetry.schema_version",
            json!({"intValue":event.schema_version.to_string()}),
        ),
    ];
    for (key, value) in [
        ("telemetry.session_id", &event.session_id),
        ("telemetry.workflow_id", &event.workflow_id),
    ] {
        if let Some(value) = value {
            values.push(attribute(key, json!({"stringValue":value})));
        }
    }
    values
}

fn category_name(category: Category) -> &'static str {
    match category {
        Category::EssentialOperations => "essential_operations",
        Category::Diagnostics => "diagnostics",
        Category::Performance => "performance",
        Category::UsageAggregate => "usage_aggregate",
        Category::UsageSession => "usage_session",
        Category::ExtendedSupport => "extended_support",
    }
}

fn attribute(key: &str, value: Value) -> Value {
    json!({"key":key,"value":value})
}

fn any_value(value: &Value) -> Result<Value, TransportError> {
    match value {
        Value::String(value) => Ok(json!({"stringValue":value})),
        Value::Bool(value) => Ok(json!({"boolValue":value})),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Ok(json!({"intValue":integer.to_string()}))
            } else if value.is_u64() {
                // AnyValue exposes int64, not uint64. Promoting a large unsigned
                // integer to double could silently lose precision.
                Err(invalid("OTLP integer property exceeds int64 range"))
            } else if let Some(number) = value.as_f64().filter(|number| number.is_finite()) {
                Ok(json!({"doubleValue":number}))
            } else {
                Err(invalid("unsupported OTLP scalar property"))
            }
        }
        _ => Err(invalid("unsupported OTLP scalar property")),
    }
}

fn required_string<'a>(event: &'a Event, property: &str) -> Result<&'a str, TransportError> {
    event
        .properties
        .get(property)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing OTLP instrument name"))
}

fn required_number(event: &Event, property: &str) -> Result<f64, TransportError> {
    event
        .properties
        .get(property)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| invalid("missing or invalid OTLP measurement"))
}

fn invalid(message: &str) -> TransportError {
    TransportError::permanent(message)
}

struct Budget(usize);

impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("OTLP encoding budget exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn charge(budget: &mut Budget, value: &impl Serialize) -> Result<(), TransportError> {
    serde_json::to_writer(budget, value)
        .map_err(|_| invalid("OTLP payload exceeds encoding budget"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use telemetry_schema::{new_id, Properties};

    fn event(name: &str, category: Category, properties: Value) -> Event {
        Event {
            schema_version: 1,
            event_id: new_id(),
            app_id: "example.app".into(),
            app_version: "1.0.0".into(),
            platform: "linux".into(),
            sdk_version: "0.1.0".into(),
            category,
            event_name: name.into(),
            occurred_at: 1_700_000_000_123,
            properties: serde_json::from_value(properties).unwrap(),
            session_id: None,
            workflow_id: None,
        }
    }

    fn attribute_value<'a>(attributes: &'a Value, key: &str) -> &'a Value {
        &attributes
            .as_array()
            .unwrap()
            .iter()
            .find(|attribute| attribute["key"] == key)
            .unwrap()["value"]
    }

    #[test]
    fn routes_each_record_once_and_uses_otlp_resource_identity() {
        let events = vec![
            event(
                "span",
                Category::Performance,
                json!({"name":"document.ocr","duration_ms":4.25}),
            ),
            event(
                "action",
                Category::UsageAggregate,
                json!({"id":"search.submit","result":"success"}),
            ),
            event(
                "metric.increment",
                Category::UsageAggregate,
                json!({"name":"documents.imported","amount":3}),
            ),
        ];
        let requests = otlp_requests(&events).unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .map(|request| request.signal)
                .collect::<Vec<_>>(),
            [OtlpSignal::Logs, OtlpSignal::Metrics, OtlpSignal::Traces]
        );
        assert_eq!(requests[0].path, "/v1/logs");
        assert_eq!(requests[1].path, "/v1/metrics");
        assert_eq!(requests[2].path, "/v1/traces");
        let actual: BTreeSet<_> = requests
            .iter()
            .flat_map(|request| &request.event_ids)
            .collect();
        assert_eq!(actual, events.iter().map(|event| &event.event_id).collect());
        let resource = &requests[0].body["resourceLogs"][0]["resource"]["attributes"];
        assert_eq!(
            attribute_value(resource, "service.name")["stringValue"],
            "example.app"
        );
        assert_eq!(
            attribute_value(resource, "service.version")["stringValue"],
            "1.0.0"
        );
        assert_eq!(
            requests[0].body["resourceLogs"][0]["scopeLogs"][0]["scope"]["name"],
            "telemetry-cargo"
        );
    }

    #[test]
    fn logs_preserve_scalar_types_and_only_redacted_errors() {
        let custom = event(
            "custom.action",
            Category::UsageAggregate,
            json!({"enabled":true,"attempt":42,"elapsed":1.25,"result":"success"}),
        );
        let error = event(
            "error",
            Category::Diagnostics,
            json!({"code":"import.failed","message":"[redacted]"}),
        );
        let requests = otlp_requests(&[custom, error]).unwrap();
        let records = &requests[0].body["resourceLogs"][0]["scopeLogs"][0]["logRecords"];
        let properties = &records[0]["attributes"];
        assert_eq!(
            attribute_value(properties, "telemetry.property.enabled"),
            &json!({"boolValue":true})
        );
        assert_eq!(
            attribute_value(properties, "telemetry.property.attempt"),
            &json!({"intValue":"42"})
        );
        assert_eq!(
            attribute_value(properties, "telemetry.property.elapsed"),
            &json!({"doubleValue":1.25})
        );
        assert_eq!(
            attribute_value(properties, "telemetry.property.result"),
            &json!({"stringValue":"success"})
        );
        assert_eq!(records[0]["severityNumber"], 9);
        assert_eq!(records[1]["severityNumber"], 17);
        assert_eq!(records[1]["severityText"], "ERROR");
        assert_eq!(records[1]["body"], json!({"stringValue":"error"}));
        assert_eq!(
            attribute_value(&records[1]["attributes"], "telemetry.property.message"),
            &json!({"stringValue":"[redacted]"})
        );
        let raw = event(
            "error",
            Category::Diagnostics,
            json!({"code":"import.failed","message":"secret@example.invalid"}),
        );
        let error = otlp_requests(&[raw]).unwrap_err();
        assert!(!error.retryable);
        assert!(!error.message.contains("secret"));
    }

    #[test]
    fn session_linkage_is_preserved_only_for_session_categories() {
        let mut screen = event(
            "screen.enter",
            Category::UsageSession,
            json!({"id":"archive.search"}),
        );
        screen.session_id = Some(new_id());
        screen.workflow_id = Some(new_id());
        let requests = otlp_requests(&[screen.clone()]).unwrap();
        let attributes =
            &requests[0].body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["attributes"];
        assert_eq!(
            attribute_value(attributes, "telemetry.session_id")["stringValue"],
            screen.session_id.as_ref().unwrap().as_str()
        );
        let mut diagnostic = event(
            "error",
            Category::Diagnostics,
            json!({"code":"import.failed"}),
        );
        diagnostic.session_id = screen.session_id;
        assert!(otlp_requests(&[diagnostic]).is_err());
    }

    #[test]
    fn integer_properties_never_silently_lose_precision() {
        let exact = event(
            "custom.counts",
            Category::UsageAggregate,
            json!({"maximum":i64::MAX,"minimum":i64::MIN}),
        );
        let requests = otlp_requests(&[exact]).unwrap();
        let attributes =
            &requests[0].body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["attributes"];
        assert_eq!(
            attribute_value(attributes, "telemetry.property.maximum")["intValue"],
            i64::MAX.to_string()
        );
        assert_eq!(
            attribute_value(attributes, "telemetry.property.minimum")["intValue"],
            i64::MIN.to_string()
        );
        let unsupported = event(
            "custom.counts",
            Category::UsageAggregate,
            json!({"count":u64::MAX}),
        );
        assert!(otlp_requests(&[unsupported]).is_err());
    }

    #[test]
    fn metrics_group_names_and_omit_unique_dimensions_and_invented_start_times() {
        let one = event(
            "metric.increment",
            Category::UsageAggregate,
            json!({"name":"documents.imported","amount":3}),
        );
        let two = event(
            "metric.increment",
            Category::UsageAggregate,
            json!({"name":"documents.imported","amount":-1.5}),
        );
        let events = [one, two];
        let requests = otlp_requests(&events).unwrap();
        let instruments = &requests[0].body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"];
        assert_eq!(instruments.as_array().unwrap().len(), 1);
        assert_eq!(instruments[0]["name"], "telemetry.documents.imported");
        let sum = &instruments[0]["sum"];
        assert_eq!(sum["aggregationTemporality"], 1);
        assert_eq!(sum["isMonotonic"], false);
        assert_eq!(sum["dataPoints"].as_array().unwrap().len(), 2);
        assert_eq!(sum["dataPoints"][1]["asDouble"], -1.5);
        assert!(sum["dataPoints"][0].get("startTimeUnixNano").is_none());
        assert_eq!(sum["dataPoints"][0]["timeUnixNano"], "1700000000123000000");
        for point in sum["dataPoints"].as_array().unwrap() {
            assert_eq!(point["attributes"].as_array().unwrap().len(), 1);
        }
        let serialized = serde_json::to_string(&requests[0].body).unwrap();
        for event in events {
            assert!(!serialized.contains(&event.event_id));
        }
        assert!(!serialized.contains("session_id"));
        assert!(!serialized.contains("workflow_id"));
    }

    #[test]
    fn spans_use_hex_ids_decimal_nanoseconds_and_monotonic_duration() {
        let mut measured = event(
            "span",
            Category::Performance,
            json!({"name":"document.ocr","duration_ms":0.125}),
        );
        measured.event_id = "00112233-4455-4677-8899-aabbccddeeff".into();
        let requests = otlp_requests(&[measured.clone()]).unwrap();
        let span = &requests[0].body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["traceId"], "00112233445546778899aabbccddeeff");
        assert_eq!(span["spanId"], "8899aabbccddeeff");
        assert_eq!(span["kind"], 1);
        assert_eq!(span["name"], "document.ocr");
        assert_eq!(span["endTimeUnixNano"], "1700000000123000000");
        assert_eq!(span["startTimeUnixNano"], "1700000000122875000");
        assert!(span.get("parentSpanId").is_none());
        assert_eq!(
            requests[0].body,
            otlp_requests(&[measured]).unwrap()[0].body
        );
        let early = event(
            "span",
            Category::Performance,
            json!({"name":"document.ocr","duration_ms":4_000_000_000_000.0}),
        );
        let request = otlp_requests(&[early]).unwrap();
        assert_eq!(
            request[0].body["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["startTimeUnixNano"],
            "0"
        );
    }

    #[test]
    fn different_resources_do_not_mix_release_or_service_identity() {
        let one = event(
            "action",
            Category::UsageAggregate,
            json!({"id":"search.submit"}),
        );
        let mut other = one.clone();
        other.event_id = new_id();
        other.app_version = "2.0.0".into();
        let requests = otlp_requests(&[one, other]).unwrap();
        assert_eq!(
            requests[0].body["resourceLogs"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn invalid_instrument_data_and_time_arithmetic_are_rejected() {
        for properties in [
            json!({"name":"operation"}),
            json!({"duration_ms":1}),
            json!({"name":"operation","duration_ms":-1}),
            json!({"name":"operation","duration_ms":f64::MAX}),
            json!({"name":"private/path","duration_ms":1}),
        ] {
            let malformed = event("span", Category::Performance, properties);
            assert!(otlp_requests(&[malformed]).is_err());
        }
        for properties in [
            json!({"name":"documents.imported"}),
            json!({"amount":1}),
            json!({"name":"documents.imported","amount":null}),
        ] {
            assert!(otlp_requests(&[event(
                "metric.increment",
                Category::UsageAggregate,
                properties
            )])
            .is_err());
        }
        let mut late = event(
            "action",
            Category::UsageAggregate,
            json!({"id":"search.submit"}),
        );
        late.occurred_at = u64::MAX;
        assert!(otlp_requests(&[late]).is_err());
    }

    #[test]
    fn empty_input_omits_all_signals_and_duplicate_ids_are_rejected() {
        assert!(otlp_requests(&[]).unwrap().is_empty());
        let action = event(
            "action",
            Category::UsageAggregate,
            json!({"id":"search.submit"}),
        );
        assert!(otlp_requests(&[action.clone(), action]).is_err());
    }

    #[test]
    fn independent_transport_size_and_count_budgets_are_enforced() {
        let action = event(
            "action",
            Category::UsageAggregate,
            json!({"id":"search.submit"}),
        );
        assert!(otlp_requests(&vec![action; MAX_EVENTS + 1])
            .unwrap_err()
            .message
            .contains("budget"));
        let properties: Properties = (0..32)
            .map(|i| (format!("property.{i}"), Value::String("x".repeat(4096))))
            .collect();
        let large: Vec<Event> = (0..33)
            .map(|_| {
                let mut large = event("custom.large", Category::UsageAggregate, json!({}));
                large.properties = properties.clone();
                large
            })
            .collect();
        assert!(otlp_requests(&large)
            .unwrap_err()
            .message
            .contains("budget"));
    }

    #[test]
    fn signal_response_names_are_standard() {
        assert_eq!(OtlpSignal::Logs.rejected_field(), "rejectedLogRecords");
        assert_eq!(OtlpSignal::Metrics.rejected_field(), "rejectedDataPoints");
        assert_eq!(OtlpSignal::Traces.rejected_field(), "rejectedSpans");
        assert_eq!(serde_json::to_value(OtlpSignal::Logs).unwrap(), "logs");
    }
}
