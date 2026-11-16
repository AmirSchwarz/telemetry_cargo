//! Node bindings: initialization and all package operations use libuv's worker pool.
use napi::bindgen_prelude::{AsyncTask, Task};
use napi::{Env, Error, JsString, Result};
use napi_derive::napi;
use serde_json::Value;
use telemetry_core::{Config, Telemetry};

#[napi]
pub struct NativeTelemetry {
    sdk: Option<Telemetry>,
}

#[napi]
pub fn create_native_telemetry(config_json: String) -> Result<AsyncTask<InitializeTask>> {
    if config_json.len() > 65_536 {
        return Err(Error::from_reason(
            "telemetry configuration exceeds size budget",
        ));
    }
    let config = serde_json::from_str(&config_json)
        .map_err(|_| Error::from_reason("invalid telemetry configuration"))?;
    Ok(AsyncTask::new(InitializeTask {
        config: Some(config),
    }))
}

#[napi]
impl NativeTelemetry {
    #[napi]
    pub fn command_async(&self, request_json: String) -> Result<AsyncTask<CommandTask>> {
        if request_json.len() > 65_536 {
            return Err(Error::from_reason("telemetry command exceeds size budget"));
        }
        let request = serde_json::from_str(&request_json)
            .map_err(|_| Error::from_reason("invalid telemetry command"))?;
        Ok(AsyncTask::new(CommandTask {
            sdk: self.sdk()?.clone(),
            request,
        }))
    }

    #[napi]
    pub fn close(&mut self, timeout_ms: u32) -> Result<AsyncTask<CommandTask>> {
        let sdk = self
            .sdk
            .take()
            .ok_or_else(|| Error::from_reason("telemetry is closed"))?;
        Ok(AsyncTask::new(CommandTask {
            sdk,
            request: serde_json::json!({"op":"shutdown", "timeout_ms":timeout_ms}),
        }))
    }
}

pub struct InitializeTask {
    config: Option<Config>,
}

impl Task for InitializeTask {
    type Output = Telemetry;
    type JsValue = NativeTelemetry;

    fn compute(&mut self) -> Result<Self::Output> {
        let config = self
            .config
            .take()
            .ok_or_else(|| Error::from_reason("initialization already completed"))?;
        Telemetry::init(config).map_err(|error| Error::from_reason(error.to_string()))
    }

    fn resolve(&mut self, _env: Env, sdk: Self::Output) -> Result<Self::JsValue> {
        Ok(NativeTelemetry { sdk: Some(sdk) })
    }
}

impl NativeTelemetry {
    fn sdk(&self) -> Result<&Telemetry> {
        self.sdk
            .as_ref()
            .ok_or_else(|| Error::from_reason("telemetry is closed"))
    }
}

fn execute(sdk: &Telemetry, request: Value) -> Result<String> {
    let response = sdk
        .command(request)
        .map_err(|error| Error::from_reason(error.to_string()))?;
    serde_json::to_string(&response)
        .map_err(|_| Error::from_reason("cannot encode telemetry response"))
}

pub struct CommandTask {
    sdk: Telemetry,
    request: Value,
}

impl Task for CommandTask {
    type Output = String;
    type JsValue = JsString;

    fn compute(&mut self) -> Result<Self::Output> {
        execute(&self.sdk, self.request.clone())
    }

    fn resolve(&mut self, env: Env, output: Self::Output) -> Result<Self::JsValue> {
        env.create_string(&output)
    }
}
