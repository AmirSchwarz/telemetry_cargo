//! CPython bindings. Every operation goes through the engine's shared protocol.
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use std::time::Duration;
use telemetry_core::{Config, Telemetry};

#[pyclass]
struct NativeTelemetry {
    inner: Option<Telemetry>,
}

#[pymethods]
impl NativeTelemetry {
    #[new]
    fn new(py: Python<'_>, config_json: &str) -> PyResult<Self> {
        if config_json.len() > 65_536 {
            return Err(PyValueError::new_err(
                "telemetry configuration exceeds size budget",
            ));
        }
        let config: Config = serde_json::from_str(config_json)
            .map_err(|_| PyValueError::new_err("invalid telemetry configuration"))?;
        let sdk = py
            .allow_threads(|| Telemetry::init(config))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok(Self { inner: Some(sdk) })
    }

    fn command(&self, py: Python<'_>, request_json: &str) -> PyResult<String> {
        if request_json.len() > 65_536 {
            return Err(PyValueError::new_err(
                "telemetry command exceeds size budget",
            ));
        }
        let sdk = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("telemetry is closed"))?
            .clone();
        let request = serde_json::from_str(request_json)
            .map_err(|_| PyValueError::new_err("invalid telemetry command"))?;
        let response = py
            .allow_threads(|| sdk.command(request))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        serde_json::to_string(&response)
            .map_err(|_| PyRuntimeError::new_err("cannot encode telemetry response"))
    }

    #[pyo3(signature = (timeout_ms=5000))]
    fn close(&mut self, py: Python<'_>, timeout_ms: u64) -> PyResult<()> {
        if let Some(sdk) = self.inner.take() {
            py.allow_threads(|| sdk.shutdown(Duration::from_millis(timeout_ms)))
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        }
        Ok(())
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<NativeTelemetry>()?;
    Ok(())
}
