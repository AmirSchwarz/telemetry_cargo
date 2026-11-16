use serde::{Deserialize, Serialize};
use std::fmt;
use telemetry_privacy::Consent;
use telemetry_queue::QueueConfig;
use telemetry_schema::Limits;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub app_id: String,
    pub app_version: String,
    pub endpoint: Option<String>,
    pub ingestion_token: Option<String>,
    pub consent: Consent,
    pub essential_operations_reviewed: bool,
    pub queue: QueueConfig,
    pub limits: Limits,
    pub idle_threshold_ms: u64,
    pub sqlite_path: Option<String>,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
}

impl Config {
    pub fn new(app_id: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_version: app_version.into(),
            ..Self::default()
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            app_version: String::new(),
            endpoint: None,
            ingestion_token: None,
            consent: Consent::default(),
            essential_operations_reviewed: false,
            queue: QueueConfig::default(),
            limits: Limits::default(),
            idle_threshold_ms: 90_000,
            sqlite_path: None,
            batch_size: 100,
            flush_interval_ms: 30_000,
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("app_id", &self.app_id)
            .field("app_version", &self.app_version)
            .field("endpoint_configured", &self.endpoint.is_some())
            .field(
                "ingestion_token",
                &self.ingestion_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "sqlite_path",
                &self.sqlite_path.as_ref().map(|_| "[redacted]"),
            )
            .field("consent", &self.consent)
            .field(
                "essential_operations_reviewed",
                &self.essential_operations_reviewed,
            )
            .field("idle_threshold_ms", &self.idle_threshold_ms)
            .field("batch_size", &self.batch_size)
            .field("flush_interval_ms", &self.flush_interval_ms)
            .finish_non_exhaustive()
    }
}
