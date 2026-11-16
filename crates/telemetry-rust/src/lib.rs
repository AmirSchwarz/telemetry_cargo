//! Application telemetry with explicit consent, bounded queues and shared bindings.
//!
//! ```no_run
//! use telemetry::{Config, Telemetry, Category, ActionResult};
//! let sdk = telemetry::init(Config::new("example.app", "1.0.0"))?;
//! sdk.set_consent(Category::UsageAggregate, true)?;
//! sdk.action("search.submit", ActionResult::Success)?;
//! # Ok::<(), telemetry::Error>(())
//! ```

pub use telemetry_core::*;
pub use telemetry_platform as platform;
pub use telemetry_privacy::Consent;
pub use telemetry_schema::{
    Category, Event, EventDefinition, Limits, Properties, PropertyRule, Registry,
};

/// Create an independent SDK instance. No global hooks or singleton are installed.
pub fn init(config: Config) -> Result<Telemetry, Error> {
    Telemetry::init(config)
}

#[cfg(feature = "tracing")]
mod tracing_adapter;
#[cfg(feature = "tracing")]
pub use tracing_adapter::TelemetryLayer;
