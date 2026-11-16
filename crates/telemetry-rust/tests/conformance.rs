use std::sync::{Arc, Mutex};
use std::time::Duration;
use telemetry::{ActionResult, Category, Config, Event, Telemetry, WorkflowOutcome};
use telemetry_transport::{Transport, TransportError};

#[derive(Default)]
struct Capture {
    events: Mutex<Vec<Event>>,
}

impl Transport for Capture {
    fn send(&self, events: &[Event], _: Duration) -> Result<(), TransportError> {
        self.events.lock().unwrap().extend_from_slice(events);
        Ok(())
    }
}

#[test]
fn optional_collection_defaults_off_including_session_state() {
    let transport = Arc::new(Capture::default());
    let sdk = Telemetry::with_transport(Config::new("conformance.app", "1.0"), transport.clone())
        .unwrap();
    sdk.action("search.submit", ActionResult::Success).unwrap();
    sdk.capture_error("import.failed", "secret@example.org")
        .unwrap();
    sdk.screen_enter("archive.search").unwrap();
    assert!(sdk.workflow_start("document.import").unwrap().is_empty());
    sdk.span("document.ocr").unwrap().finish().unwrap();
    sdk.flush(Duration::from_secs(1)).unwrap();
    assert!(transport.events.lock().unwrap().is_empty());
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    sdk.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn errors_are_redacted_and_withdrawal_purges_before_upload() {
    let transport = Arc::new(Capture::default());
    let mut config = Config::new("conformance.app", "1.0");
    config.flush_interval_ms = 60_000;
    let sdk = Telemetry::with_transport(config, transport.clone()).unwrap();
    sdk.set_consent(Category::Diagnostics, true).unwrap();
    sdk.set_consent(Category::UsageAggregate, true).unwrap();
    sdk.capture_error(
        "import.failed",
        "password=top-secret /home/alice/report.pdf https://private.invalid?q=secret",
    )
    .unwrap();
    sdk.action("search.submit", ActionResult::Success).unwrap();
    sdk.set_consent(Category::UsageAggregate, false).unwrap();
    sdk.flush(Duration::from_secs(2)).unwrap();
    let captured = transport.events.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].event_name, "error");
    let serialized = serde_json::to_string(&*captured).unwrap();
    for forbidden in [
        "top-secret",
        "alice",
        "report.pdf",
        "private.invalid",
        "search.submit",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }
    assert_eq!(captured[0].properties["message"], "[redacted]");
    assert!(captured[0].session_id.is_none());
    drop(captured);
    sdk.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn session_ids_are_ephemeral_and_workflows_have_stable_ids() {
    let transport = Arc::new(Capture::default());
    let mut config = Config::new("conformance.app", "1.0");
    config.flush_interval_ms = 60_000;
    let sdk = Telemetry::with_transport(config, transport.clone()).unwrap();
    sdk.set_consent(Category::UsageSession, true).unwrap();
    let id = sdk.workflow_start("document.import").unwrap();
    sdk.workflow_step(&id, "files_selected").unwrap();
    sdk.workflow_end(&id, WorkflowOutcome::Complete).unwrap();
    sdk.flush(Duration::from_secs(1)).unwrap();
    let first_session = {
        let events = transport.events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .all(|event| event.workflow_id.as_deref() == Some(id.as_str())));
        events[0].session_id.clone().unwrap()
    };
    sdk.set_consent(Category::UsageSession, false).unwrap();
    sdk.set_consent(Category::UsageSession, true).unwrap();
    sdk.screen_enter("archive.search").unwrap();
    sdk.flush(Duration::from_secs(1)).unwrap();
    assert_ne!(
        transport
            .events
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .session_id
            .as_deref(),
        Some(first_session.as_str())
    );
    sdk.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn config_debug_never_exposes_secrets_or_persistence_paths() {
    let mut config = Config::new("conformance.app", "1.0");
    config.ingestion_token = Some("ingestion-secret-123".into());
    config.sqlite_path = Some("/home/alice/private-queue.db".into());
    let debug = format!("{config:?}");
    assert!(!debug.contains("ingestion-secret-123"));
    assert!(!debug.contains("alice"));
}

#[test]
fn offline_flush_reports_failure_and_preserves_pending_events() {
    let sdk = Telemetry::init(Config::new("conformance.app", "1.0")).unwrap();
    sdk.set_consent(Category::UsageAggregate, true).unwrap();
    sdk.increment("documents.imported", 3.0).unwrap();
    assert!(sdk.flush(Duration::from_millis(20)).is_err());
    assert_eq!(sdk.diagnostics().queue_depth, 1);
    sdk.set_consent(Category::UsageAggregate, false).unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    sdk.shutdown(Duration::from_millis(20)).unwrap();
    assert!(sdk.action("search.submit", ActionResult::Success).is_err());
}
