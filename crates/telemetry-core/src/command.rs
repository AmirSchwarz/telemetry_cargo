use super::{
    ActionResult, Category, Error, EventDefinition, Properties, Telemetry, WorkflowOutcome,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    SetConsent {
        category: Category,
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
        result: ActionResult,
    },
    CaptureError {
        code: String,
        message: String,
    },
    Increment {
        name: String,
        amount: f64,
    },
    ScreenEnter {
        id: String,
    },
    ScreenLeave {
        id: String,
    },
    WorkflowStart {
        name: String,
    },
    WorkflowStep {
        id: String,
        step: String,
    },
    WorkflowEnd {
        id: String,
        outcome: WorkflowOutcome,
    },
    Span {
        name: String,
        duration_ms: f64,
        #[serde(default)]
        consent_epoch: Option<u64>,
    },
    Lifecycle {
        window_id: String,
        visible: bool,
        focused: bool,
    },
    Interaction {},
    Suspend {
        suspended: bool,
    },
    Checkpoint {},
    Flush {
        timeout_ms: u64,
    },
    Shutdown {
        timeout_ms: u64,
    },
    Diagnostics {},
    Aggregates {},
    Capabilities {},
}

impl Telemetry {
    /// Language-independent commands retain the same validation and privacy semantics.
    pub fn command(&self, command: Value) -> Result<Value, Error> {
        // The FFI has a byte cap as well. This cap protects in-process dynamic adapters.
        if serde_json::to_vec(&command)
            .map_err(|_| Error::InvalidInput("invalid command"))?
            .len()
            > 1_048_576
        {
            return Err(Error::InvalidInput("command exceeds size budget"));
        }
        let command: Command = serde_json::from_value(command)
            .map_err(|_| Error::InvalidInput("invalid command fields or operation"))?;
        match command {
            Command::SetConsent { category, enabled } => self.set_consent(category, enabled)?,
            Command::Event { name, properties } => self.event(&name, properties)?,
            Command::RegisterEvent { definition } => self.register_event(definition)?,
            Command::Action { id, result } => self.action(&id, result)?,
            Command::CaptureError { code, message } => self.capture_error(&code, &message)?,
            Command::Increment { name, amount } => self.increment(&name, amount)?,
            Command::ScreenEnter { id } => self.screen_enter(&id)?,
            Command::ScreenLeave { id } => self.screen_leave(&id)?,
            Command::WorkflowStart { name } => {
                return self.workflow_start(&name).map(|id| json!({ "id": id }))
            }
            Command::WorkflowStep { id, step } => self.workflow_step(&id, &step)?,
            Command::WorkflowEnd { id, outcome } => self.workflow_end(&id, outcome)?,
            Command::Span {
                name,
                duration_ms,
                consent_epoch,
            } => self
                .state()?
                .record_span(&name, duration_ms, consent_epoch)?,
            Command::Lifecycle {
                window_id,
                visible,
                focused,
            } => self.lifecycle(&window_id, visible, focused)?,
            Command::Interaction {} => self.interaction()?,
            Command::Suspend { suspended } => self.suspend(suspended)?,
            Command::Checkpoint {} => self.checkpoint()?,
            Command::Flush { timeout_ms } => self.flush(command_timeout(timeout_ms)?)?,
            Command::Shutdown { timeout_ms } => self.shutdown(command_timeout(timeout_ms)?)?,
            Command::Diagnostics {} => {
                return serde_json::to_value(self.diagnostics())
                    .map_err(|_| Error::InvalidInput("cannot serialize diagnostics"))
            }
            Command::Aggregates {} => {
                return serde_json::to_value(self.aggregates())
                    .map_err(|_| Error::InvalidInput("cannot serialize aggregates"))
            }
            Command::Capabilities {} => return Ok(self.capabilities()),
        }
        Ok(json!({}))
    }
}

fn command_timeout(timeout_ms: u64) -> Result<Duration, Error> {
    if timeout_ms > 86_400_000 {
        return Err(Error::InvalidInput("timeout must not exceed one day"));
    }
    Ok(Duration::from_millis(timeout_ms))
}
