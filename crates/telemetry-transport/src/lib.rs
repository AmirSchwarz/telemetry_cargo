//! Batch delivery for a telemetry exporter worker.
//!
//! This crate never schedules requests itself. `send` is blocking and must run
//! on an exporter worker, outside UI, async-runtime, and interpreter threads.
//! Enable `http` for HTTPS exporters. The core uses `send_report` to consume
//! independently completed OTLP signals. Delivery remains at least once when a
//! connection fails after server acceptance; a collector need not deduplicate.

use std::{fmt, time::Duration};
use telemetry_schema::Event;

/// A delivery attempt. Implementations must honor the supplied timeout and
/// must not expose event payloads or credentials in errors.
pub trait Transport: Send + Sync {
    fn send(&self, events: &[Event], timeout: Duration) -> Result<(), TransportError>;

    /// Report independently completed signal requests without replaying them.
    /// Existing custom transports retain all-or-nothing delivery by default.
    fn send_report(&self, events: &[Event], timeout: Duration) -> DeliveryReport {
        match self.send(events, timeout) {
            Ok(()) => DeliveryReport {
                consumed: events.iter().map(|event| event.event_id.clone()).collect(),
                ..DeliveryReport::default()
            },
            Err(error) => DeliveryReport {
                error: Some(error),
                ..DeliveryReport::default()
            },
        }
    }
}

/// Records to remove from the queue, plus any definitive losses or export error.
/// `consumed` includes accepted records and definitively rejected records.
#[derive(Clone, Debug, Default)]
pub struct DeliveryReport {
    pub consumed: Vec<String>,
    pub rejected: usize,
    pub error: Option<TransportError>,
}

/// Wire protocol used by the configured HTTP exporter.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportProtocol {
    #[default]
    OtlpHttpJson,
    LegacyJson,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

impl TransportError {
    fn permanent(message: &str) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            retry_after: None,
        }
    }
}

/// Maximum serialized request size, independent of queue configuration.
pub const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
/// A single attempt can never hold an exporter worker longer than this.
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub use http::HttpTransport;

mod otlp;
pub use otlp::{otlp_requests, OtlpRequest, OtlpSignal};

mod otlp_response;
pub use otlp_response::parse_otlp_response;

#[cfg(feature = "http")]
mod otlp_http;
#[cfg(feature = "http")]
pub use otlp_http::OtlpHttpTransport;
