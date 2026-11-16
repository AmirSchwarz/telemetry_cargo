//! Batch delivery for a telemetry exporter worker.
//!
//! This crate never schedules requests itself. `send` is blocking and must run
//! on an exporter worker, outside UI, async-runtime, and interpreter threads.
//! Enable `http` for the HTTPS implementation. A successful response acknowledges
//! every event in the submitted batch; the gateway must deduplicate event IDs.

use std::{fmt, time::Duration};
use telemetry_schema::Event;

/// A delivery attempt. Implementations must honor the supplied timeout and
/// must not expose event payloads or credentials in errors.
pub trait Transport: Send + Sync {
    fn send(&self, events: &[Event], timeout: Duration) -> Result<(), TransportError>;
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

#[cfg(feature = "http")]
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
