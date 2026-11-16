use crate::Telemetry;
use tracing::{
    field::{Field, Visit},
    Event, Subscriber,
};
use tracing_subscriber::{
    layer::{Context, Layer},
    registry::LookupSpan,
};

/// An opt-in tracing bridge. Only explicit `telemetry_code` fields on ERROR
/// events become telemetry. Messages, debug values and arbitrary fields are
/// never visited or exported. This does not install a global subscriber.
#[derive(Clone)]
pub struct TelemetryLayer {
    sdk: Telemetry,
}

impl TelemetryLayer {
    pub fn new(sdk: Telemetry) -> Self {
        Self { sdk }
    }
}

#[derive(Default)]
struct CodeVisitor {
    code: Option<String>,
}

impl Visit for CodeVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "telemetry_code" && telemetry_schema::validate_id(value).is_ok() {
            self.code = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

impl<S> Layer<S> for TelemetryLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if event.metadata().level() == &tracing::Level::ERROR {
            let mut visitor = CodeVisitor::default();
            event.record(&mut visitor);
            if let Some(code) = visitor.code {
                let _ = self.sdk.capture_error(&code, "");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Category, Config};
    use tracing_subscriber::prelude::*;

    #[test]
    fn exports_only_explicit_error_codes() {
        let sdk = Telemetry::init(Config::new("tracing.test", "1.0")).unwrap();
        sdk.set_consent(Category::Diagnostics, true).unwrap();
        let subscriber = tracing_subscriber::registry().with(TelemetryLayer::new(sdk.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(message = "secret@example.org");
            tracing::info!(telemetry_code = "ignored.info");
            tracing::error!(
                telemetry_code = "https://secret.invalid/path",
                message = "ignored"
            );
            tracing::error!(
                telemetry_code = "import.failed",
                message = "secret@example.org"
            );
        });
        assert_eq!(sdk.diagnostics().queue_depth, 1);
    }
}
