//! Thread-free browser subset using the canonical native schema, gate and queue.
//! JavaScript provides wall time; durations must come from performance.now().
use serde::Deserialize;
use serde_json::{json, Value};
use telemetry_privacy::{Consent, PrivacyGate};
use telemetry_queue::{EventQueue, MemoryQueue, QueueConfig};
use telemetry_schema::{new_id, validate_id, Event, EventDefinition, Limits, Properties, Registry};
use wasm_bindgen::prelude::*;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BrowserConfig {
    app_id: String,
    app_version: String,
    consent: Consent,
    essential_operations_reviewed: bool,
    queue: QueueConfig,
    limits: Limits,
}

#[wasm_bindgen]
pub struct WasmTelemetry {
    app_id: String,
    app_version: String,
    gate: PrivacyGate,
    registry: Registry,
    limits: Limits,
    queue: MemoryQueue,
    closed: bool,
    accepted: u64,
    dropped: u64,
}

fn failure() -> JsValue {
    JsValue::from_str("invalid or unsupported telemetry operation")
}

#[wasm_bindgen]
impl WasmTelemetry {
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str) -> Result<WasmTelemetry, JsValue> {
        if config_json.len() > 65_536 {
            return Err(failure());
        }
        let config: BrowserConfig = serde_json::from_str(config_json).map_err(|_| failure())?;
        validate_id(&config.app_id).map_err(|_| failure())?;
        if config.app_version.is_empty()
            || config.app_version.len() > 128
            || config.app_version.chars().any(char::is_control)
        {
            return Err(failure());
        }
        config.limits.validate().map_err(|_| failure())?;
        let queue = MemoryQueue::new(config.queue).map_err(|_| failure())?;
        Ok(Self {
            app_id: config.app_id,
            app_version: config.app_version,
            gate: PrivacyGate::new(config.consent, config.essential_operations_reviewed),
            registry: Registry::default(),
            limits: config.limits,
            queue,
            closed: false,
            accepted: 0,
            dropped: 0,
        })
    }

    /// `now_ms` is supplied by Date.now(), never used for duration measurement.
    pub fn command(&mut self, request_json: &str, now_ms: f64) -> Result<String, JsValue> {
        if request_json.len() > 65_536 {
            return Err(failure());
        }
        if !now_ms.is_finite() || now_ms < 0.0 || now_ms > 9_007_199_254_740_991.0 {
            return Err(failure());
        }
        let command: Command = serde_json::from_str(request_json).map_err(|_| failure())?;
        let now_ms = now_ms as u64;
        let result = self.execute(command, now_ms)?;
        serde_json::to_string(&result).map_err(|_| failure())
    }

    /// Validate bounded OTLP responses through the same exact ProtoJSON parser
    /// as native transport. Passing text directly avoids command JSON escaping
    /// overhead and JavaScript numeric precision loss.
    pub fn response_rejected(
        &self,
        signal: &str,
        body: &str,
        submitted: usize,
    ) -> Result<usize, JsValue> {
        let signal = match signal {
            "logs" => telemetry_transport::OtlpSignal::Logs,
            "metrics" => telemetry_transport::OtlpSignal::Metrics,
            "traces" => telemetry_transport::OtlpSignal::Traces,
            _ => return Err(failure()),
        };
        telemetry_transport::parse_otlp_response(body.as_bytes(), signal, submitted)
            .map_err(|_| failure())
    }
}

impl WasmTelemetry {
    fn emit(&mut self, name: &str, mut properties: Properties, now_ms: u64) -> Result<(), JsValue> {
        let definition = self.registry.definition(name).ok_or_else(failure)?;
        if definition.category == telemetry_schema::Category::UsageSession {
            return Err(JsValue::from_str(
                "browser session telemetry is unsupported",
            ));
        }
        if !self.gate.enabled(definition.category) {
            return Ok(());
        }
        self.gate.sanitize(definition, &mut properties);
        let event = Event {
            schema_version: 1,
            event_id: new_id(),
            app_id: self.app_id.clone(),
            app_version: self.app_version.clone(),
            platform: "browser".to_owned(),
            sdk_version: env!("CARGO_PKG_VERSION").to_owned(),
            category: definition.category,
            event_name: name.to_owned(),
            occurred_at: now_ms,
            properties,
            session_id: None,
            workflow_id: None,
        };
        self.registry
            .validate(&event, &self.limits)
            .map_err(|_| failure())?;
        self.dropped += self.queue.enqueue(event, now_ms).map_err(|_| failure())? as u64;
        self.accepted += 1;
        Ok(())
    }

    fn execute(&mut self, command: Command, now_ms: u64) -> Result<Value, JsValue> {
        if self.closed {
            return Err(JsValue::from_str("telemetry is closed"));
        }
        match command {
            Command::SetConsent { category, enabled } => {
                if category == telemetry_schema::Category::UsageSession && enabled {
                    return Err(JsValue::from_str(
                        "browser session telemetry is unsupported",
                    ));
                }
                self.gate.set_consent(category, enabled);
                self.queue
                    .retain_categories(&self.gate.allowed_categories())
                    .map_err(|_| failure())?;
            }
            Command::Event { name, properties } => self.emit(&name, properties, now_ms)?,
            Command::RegisterEvent { definition } => {
                // Session operations are not implemented in the browser subset.
                if definition.category == telemetry_schema::Category::UsageSession {
                    return Err(JsValue::from_str(
                        "browser session telemetry is unsupported",
                    ));
                }
                self.registry.register(definition).map_err(|_| failure())?;
            }
            Command::Action { id, result } => {
                self.emit("action", props(json!({"id":id,"result":result}))?, now_ms)?
            }
            Command::CaptureError { code, message } => self.emit(
                "error",
                props(json!({"code":code,"message":message}))?,
                now_ms,
            )?,
            Command::Increment { name, amount } => self.emit(
                "metric.increment",
                props(json!({"name":name,"amount":amount}))?,
                now_ms,
            )?,
            Command::Span { name, duration_ms } => {
                if !duration_ms.is_finite() || duration_ms < 0.0 {
                    return Err(failure());
                }
                self.emit(
                    "span",
                    props(json!({"name":name,"duration_ms":duration_ms}))?,
                    now_ms,
                )?;
            }
            Command::Batch { limit } => {
                if limit == 0 || limit > 256 {
                    return Err(failure());
                }
                return Ok(
                    json!({"schema_version":1,"events":self.queue.batch(limit,now_ms).map_err(|_| failure())?}),
                );
            }
            Command::OtlpBatch { limit } => {
                if limit == 0 || limit > 256 {
                    return Err(failure());
                }
                let events = self.queue.batch(limit, now_ms).map_err(|_| failure())?;
                let requests =
                    telemetry_transport::otlp_requests(&events).map_err(|_| failure())?;
                return serde_json::to_value(requests).map_err(|_| failure());
            }
            Command::Acknowledge { ids, rejected } => {
                if ids.len() > 256
                    || rejected > ids.len()
                    || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
                {
                    return Err(failure());
                }
                self.queue.acknowledge(&ids).map_err(|_| failure())?;
                self.dropped = self.dropped.saturating_add(rejected as u64);
            }
            Command::Diagnostics {} => {
                return Ok(
                    json!({"queue_depth":self.queue.len(),"queued_bytes":self.queue.byte_len(),"accepted":self.accepted,"dropped":self.dropped}),
                );
            }
            Command::Capabilities {} => {
                return Ok(json!({
                    "binding":"wasm", "memory_queue":true, "sqlite":false, "indexeddb":false,
                    "lifecycle":false, "workflows":false, "screens":false, "process_metrics":false,
                    "crash_recovery":false, "browser_adapter_transport":true,
                    "otlp_http_json":true, "legacy_json":true
                }))
            }
            Command::Shutdown {} => {
                self.closed = true;
            }
        }
        Ok(json!({}))
    }
}

fn props(value: Value) -> Result<Properties, JsValue> {
    serde_json::from_value(value).map_err(|_| failure())
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    SetConsent {
        category: telemetry_schema::Category,
        enabled: bool,
    },
    Event {
        name: String,
        properties: Properties,
    },
    RegisterEvent {
        definition: EventDefinition,
    },
    Action {
        id: String,
        result: String,
    },
    CaptureError {
        code: String,
        message: String,
    },
    Increment {
        name: String,
        amount: f64,
    },
    Span {
        name: String,
        duration_ms: f64,
    },
    Batch {
        limit: usize,
    },
    OtlpBatch {
        limit: usize,
    },
    Acknowledge {
        ids: Vec<String>,
        #[serde(default)]
        rejected: usize,
    },
    Diagnostics {},
    Capabilities {},
    Shutdown {},
}
