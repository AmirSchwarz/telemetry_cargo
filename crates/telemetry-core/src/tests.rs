use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc;
use std::thread;

#[derive(Default)]
struct RecordingTransport {
    events: Mutex<Vec<Event>>,
    threads: Mutex<Vec<thread::ThreadId>>,
}

impl Transport for RecordingTransport {
    fn send(&self, events: &[Event], _timeout: Duration) -> Result<(), TransportError> {
        self.events.lock().unwrap().extend_from_slice(events);
        self.threads.lock().unwrap().push(thread::current().id());
        Ok(())
    }
}

fn config() -> Config {
    Config::new("example.app", "1.0.0")
}

fn enabled_config() -> Config {
    let mut config = config();
    for category in Category::ALL {
        config.consent.set(category, true);
    }
    config
}

#[test]
fn defaults_do_not_collect_state_even_for_invalid_private_values() {
    let sdk = Telemetry::init(config()).unwrap();
    sdk.action("private/path", ActionResult::Success).unwrap();
    sdk.capture_error("error", "password=secret").unwrap();
    sdk.increment("counter", f64::NAN).unwrap();
    sdk.screen_enter("private/path").unwrap();
    sdk.lifecycle("private/path", true, true).unwrap();
    assert!(sdk.workflow_start("private/path").unwrap().is_empty());
    let span = sdk.span("private/path").unwrap();
    assert!(span.sdk.is_none());
    assert!(span.name.is_none());
    let state = sdk.state().unwrap();
    assert!(state.session_id.is_none());
    assert!(state.lifecycle.is_none());
    assert!(state.screens.is_empty());
    assert!(state.workflows.is_empty());
    assert!(state.aggregates.counters.is_empty());
    assert!(state.aggregates.spans.is_empty());
    assert!(state.queue.is_empty());
}

#[test]
fn errors_are_redacted_and_upload_happens_on_worker() {
    let transport = Arc::new(RecordingTransport::default());
    let sdk = Telemetry::with_transport(enabled_config(), transport.clone()).unwrap();
    sdk.capture_error(
        "import.failed",
        "Bearer token=abc /home/alice/private https://secret.example/doc",
    )
    .unwrap();
    sdk.flush(Duration::from_secs(1)).unwrap();
    let events = transport.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].properties["message"], "[redacted]");
    assert!(events[0].session_id.is_none());
    assert!(!serde_json::to_string(&*events).unwrap().contains("alice"));
    assert_ne!(transport.threads.lock().unwrap()[0], thread::current().id());
    assert_eq!(sdk.diagnostics().queue_depth, 0);
}

#[test]
fn withdrawal_purges_queue_and_state_and_regrant_creates_new_session() {
    let sdk = Telemetry::init(enabled_config()).unwrap();
    sdk.screen_enter("home").unwrap();
    let workflow = sdk.workflow_start("import").unwrap();
    sdk.increment("imports", 3.0).unwrap();
    let old_session = sdk.state().unwrap().session_id.clone().unwrap();
    let span = sdk.span("operation").unwrap();
    sdk.set_consent(Category::UsageSession, false).unwrap();
    {
        let state = sdk.state().unwrap();
        assert!(state.session_id.is_none());
        assert!(state.lifecycle.is_none());
        assert!(state.screens.is_empty());
        assert!(state.workflows.is_empty());
        assert_eq!(state.queue.len(), 1);
    }
    sdk.set_consent(Category::UsageSession, true).unwrap();
    assert_ne!(
        old_session,
        sdk.state().unwrap().session_id.clone().unwrap()
    );
    assert!(sdk.workflow_step(&workflow, "next").is_err());
    sdk.set_consent(Category::Performance, false).unwrap();
    sdk.set_consent(Category::Performance, true).unwrap();
    span.finish().unwrap();
    assert!(sdk.aggregates().spans.is_empty());
    sdk.set_consent(Category::UsageAggregate, false).unwrap();
    assert!(sdk.aggregates().counters.is_empty());
    assert_eq!(sdk.diagnostics().queue_depth, 0);
}

#[test]
fn custom_events_and_commands_reject_unknown_fields() {
    let sdk = Telemetry::init(enabled_config()).unwrap();
    assert_eq!(
        sdk.event("custom", Properties::new()),
        Err(Error::UnknownEvent)
    );
    sdk.register_event(EventDefinition {
        name: "custom".to_owned(),
        category: Category::UsageAggregate,
        properties: BTreeMap::from([("result".to_owned(), PropertyRule::Boolean)]),
    })
    .unwrap();
    sdk.event("custom", props([("result", json!(true))]))
        .unwrap();
    assert!(sdk
        .event("custom", props([("secret", json!("password"))]))
        .is_err());
    assert!(sdk
        .command(json!({"op":"action","id":"search","result":"success","content":"secret"}))
        .is_err());
    assert!(sdk.command(json!({"op":"unknown"})).is_err());
    for op in [
        "diagnostics",
        "capabilities",
        "aggregates",
        "interaction",
        "checkpoint",
    ] {
        assert!(sdk.command(json!({"op":op,"extra":true})).is_err());
    }
    assert!(serde_json::from_value::<Config>(
        json!({"app_id":"test","app_version":"1","unexpected":true})
    )
    .is_err());
}

#[test]
fn aggregate_maps_are_bounded_and_consent_scoped() {
    let sdk = Telemetry::init(enabled_config()).unwrap();
    sdk.increment("imports", 3.0).unwrap();
    sdk.increment("imports", -1.0).unwrap();
    sdk.record_span("ocr", 5.0).unwrap();
    sdk.record_span("ocr", 15.0).unwrap();
    let aggregates = sdk.aggregates();
    assert_eq!(aggregates.counters["imports"].sum, 2.0);
    assert_eq!(aggregates.counters["imports"].count, 2);
    assert_eq!(aggregates.spans["ocr"].count, 2);
    assert_eq!(aggregates.spans["ocr"].sum_ms, 20.0);
    assert_eq!(aggregates.spans["ocr"].buckets[1], 1);
    assert_eq!(aggregates.spans["ocr"].buckets[3], 1);
    for index in 1..256 {
        sdk.increment(&format!("metric_{index}"), 1.0).unwrap();
    }
    assert_eq!(sdk.increment("overflow", 1.0), Err(Error::Capacity));
    assert_eq!(sdk.aggregates().counters.len(), 256);
    sdk.set_consent(Category::Performance, false).unwrap();
    assert!(sdk.aggregates().spans.is_empty());
    assert_eq!(sdk.aggregates().counters.len(), 256);
}

#[test]
fn finite_inputs_cannot_overflow_counter_or_span_aggregates() {
    let sdk = Telemetry::init(enabled_config()).unwrap();
    sdk.increment("large_counter", f64::MAX).unwrap();
    sdk.record_span("large_span", f64::MAX).unwrap();
    let before = sdk.diagnostics();

    assert_eq!(
        sdk.increment("large_counter", f64::MAX),
        Err(Error::InvalidInput(
            "counter aggregate exceeds numeric budget"
        ))
    );
    assert_eq!(
        sdk.record_span("large_span", f64::MAX),
        Err(Error::InvalidInput("span aggregate exceeds numeric budget"))
    );

    let aggregates = sdk.aggregates();
    let counter = &aggregates.counters["large_counter"];
    assert_eq!(counter.count, 1);
    assert_eq!(counter.sum, f64::MAX);
    let span = &aggregates.spans["large_span"];
    assert_eq!(span.count, 1);
    assert_eq!(span.sum_ms, f64::MAX);
    assert_eq!(span.min_ms, f64::MAX);
    assert_eq!(span.max_ms, f64::MAX);
    assert_eq!(span.buckets, [0, 0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(span.buckets.iter().sum::<u64>(), span.count);
    let after = sdk.diagnostics();
    assert_eq!(after.accepted, before.accepted);
    assert_eq!(after.queue_depth, before.queue_depth);
    assert_eq!(after.queued_bytes, before.queued_bytes);
    assert!(!serde_json::to_string(&aggregates).unwrap().contains("null"));

    sdk.increment("negative_counter", -f64::MAX).unwrap();
    assert!(sdk.increment("negative_counter", -f64::MAX).is_err());
    assert_eq!(sdk.aggregates().counters["negative_counter"].sum, -f64::MAX);
    assert_eq!(sdk.aggregates().counters["negative_counter"].count, 1);
}

#[test]
fn adapter_span_epoch_prevents_cross_consent_collection() {
    let sdk = Telemetry::init(enabled_config()).unwrap();
    let epoch = sdk.diagnostics().consent_epoch;
    sdk.set_consent(Category::Performance, false).unwrap();
    sdk.set_consent(Category::Performance, true).unwrap();
    sdk.command(json!({"op":"span","name":"ocr","duration_ms":10,"consent_epoch":epoch}))
        .unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    sdk.command(json!({"op":"span","name":"ocr","duration_ms":10,"consent_epoch":sdk.diagnostics().consent_epoch})).unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 1);
}

#[test]
fn no_transport_and_shutdown_keep_pending_queue_and_close_collection() {
    let sdk = Telemetry::init(enabled_config()).unwrap();
    sdk.action("search", ActionResult::Success).unwrap();
    assert_eq!(sdk.flush(Duration::from_millis(1)), Err(Error::NoTransport));
    assert_eq!(
        sdk.shutdown(Duration::from_millis(1)),
        Err(Error::NoTransport)
    );
    assert_eq!(sdk.diagnostics().queue_depth, 1);
    assert_eq!(
        sdk.action("search", ActionResult::Success),
        Err(Error::Closed)
    );
    sdk.shutdown(Duration::ZERO).unwrap();
}

struct FailingTransport {
    calls: AtomicUsize,
    retryable: bool,
}

impl Transport for FailingTransport {
    fn send(&self, _events: &[Event], _timeout: Duration) -> Result<(), TransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(TransportError {
            message: "do not expose body".to_owned(),
            retryable: self.retryable,
            retry_after: Some(Duration::from_secs(60)),
        })
    }
}

#[test]
fn retry_after_and_flush_deadline_keep_payload_for_later() {
    let transport = Arc::new(FailingTransport {
        calls: AtomicUsize::new(0),
        retryable: true,
    });
    let sdk = Telemetry::with_transport(enabled_config(), transport.clone()).unwrap();
    sdk.action("search", ActionResult::Success).unwrap();
    let started = Instant::now();
    assert_eq!(
        sdk.flush(Duration::from_millis(30)),
        Err(Error::DeadlineExceeded)
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(sdk.diagnostics().queue_depth, 1);
    assert_eq!(sdk.diagnostics().exporter_failures, 1);
    sdk.set_consent(Category::UsageAggregate, false).unwrap();
    sdk.flush(Duration::ZERO).unwrap();
}

#[test]
fn permanent_export_failure_returns_without_losing_batch() {
    let transport = Arc::new(FailingTransport {
        calls: AtomicUsize::new(0),
        retryable: false,
    });
    let sdk = Telemetry::with_transport(enabled_config(), transport).unwrap();
    sdk.action("search", ActionResult::Success).unwrap();
    assert_eq!(
        sdk.flush(Duration::from_secs(1)),
        Err(Error::Transport { retryable: false })
    );
    assert_eq!(sdk.diagnostics().queue_depth, 1);
}

struct ControlledTransport {
    started: mpsc::SyncSender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
    calls: AtomicUsize,
}

impl Transport for ControlledTransport {
    fn send(&self, _events: &[Event], timeout: Duration) -> Result<(), TransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.send(()).unwrap();
        let (lock, changed) = &*self.release;
        let guard = lock.lock().unwrap();
        let _ = changed
            .wait_timeout_while(guard, timeout, |released| !*released)
            .unwrap();
        Ok(())
    }
}

#[test]
fn withdrawal_waits_for_inflight_send_while_collection_remains_responsive() {
    let (tx, rx) = mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let transport = Arc::new(ControlledTransport {
        started: tx,
        release: release.clone(),
        calls: AtomicUsize::new(0),
    });
    let sdk = Telemetry::with_transport(enabled_config(), transport.clone()).unwrap();
    sdk.action("first", ActionResult::Success).unwrap();
    let flush_sdk = sdk.clone();
    let flush = thread::spawn(move || flush_sdk.flush(Duration::from_secs(2)));
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let revoked = Arc::new(AtomicBool::new(false));
    let revoke_sdk = sdk.clone();
    let revoke_flag = revoked.clone();
    let revoke = thread::spawn(move || {
        revoke_sdk
            .set_consent(Category::UsageAggregate, false)
            .unwrap();
        revoke_flag.store(true, Ordering::SeqCst);
    });
    let before = Instant::now();
    sdk.action("second", ActionResult::Success).unwrap();
    assert!(before.elapsed() < Duration::from_millis(100));
    assert!(!revoked.load(Ordering::SeqCst));
    // Removing consent must run before any subsequent export. Queue stays under
    // the same upload mutex, so whichever contender wins still uses current consent.
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    revoke.join().unwrap();
    flush.join().unwrap().unwrap();
    sdk.action("third", ActionResult::Success).unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    assert!(transport.calls.load(Ordering::SeqCst) <= 2);
}

#[test]
fn config_debug_never_exposes_token_or_storage_path() {
    let mut config = config();
    config.ingestion_token = Some("VERY_SECRET_TOKEN".to_owned());
    config.sqlite_path = Some("/home/private/location".to_owned());
    config.otlp_headers.insert(
        "authorization".to_owned(),
        "VERY_SECRET_CLICKSTACK_KEY".to_owned(),
    );
    let debug = format!("{config:?}");
    assert!(!debug.contains("VERY_SECRET_TOKEN"));
    assert!(!debug.contains("/home/private/location"));
    assert!(!debug.contains("VERY_SECRET_CLICKSTACK_KEY"));
    assert!(!debug.contains("authorization"));
    assert!(debug.contains("[redacted]"));
}

#[test]
fn otlp_is_default_and_configuration_round_trips() {
    let default = config();
    assert_eq!(default.export_protocol, ExportProtocol::OtlpHttpJson);
    assert!(!default.allow_insecure_loopback);
    assert!(default.otlp_headers.is_empty());
    let parsed: Config = serde_json::from_value(json!({
        "app_id":"example.app", "app_version":"1.0",
        "export_protocol":"otlp_http_json", "allow_insecure_loopback":true,
        "otlp_headers":{"authorization":"public-test-ingestion-key"}
    }))
    .unwrap();
    assert_eq!(parsed.export_protocol, ExportProtocol::OtlpHttpJson);
    assert!(parsed.allow_insecure_loopback);
    assert_eq!(
        parsed.otlp_headers["authorization"],
        "public-test-ingestion-key"
    );
    let encoded = serde_json::to_value(parsed).unwrap();
    assert_eq!(encoded["export_protocol"], "otlp_http_json");
    assert!(serde_json::from_value::<Config>(json!({"export_protocol":"otlp_grpc"})).is_err());
}

#[test]
fn legacy_protocol_is_explicit_and_rejects_otlp_options() {
    let mut legacy = config();
    legacy.export_protocol = ExportProtocol::LegacyJson;
    let encoded = serde_json::to_value(&legacy).unwrap();
    assert_eq!(encoded["export_protocol"], "legacy_json");
    Telemetry::init(legacy.clone()).unwrap();
    legacy.allow_insecure_loopback = true;
    assert!(matches!(
        Telemetry::init(legacy.clone()),
        Err(Error::InvalidConfig(_))
    ));
    legacy.allow_insecure_loopback = false;
    legacy
        .otlp_headers
        .insert("authorization".to_owned(), "public-test-key".to_owned());
    assert!(matches!(
        Telemetry::init(legacy),
        Err(Error::InvalidConfig(_))
    ));
}

#[cfg(feature = "http")]
#[test]
fn configured_exporters_select_protocol_and_discard_header_secrets() {
    let mut otlp = config();
    otlp.endpoint = Some("https://collector.example.com".to_owned());
    otlp.otlp_headers
        .insert("authorization".to_owned(), "public-test-key".to_owned());
    let sdk = Telemetry::init(otlp).unwrap();
    assert!(sdk.state().unwrap().config.otlp_headers.is_empty());
    assert_eq!(sdk.capabilities()["export_protocol"], "otlp_http_json");
    assert_eq!(sdk.capabilities()["otlp_http_json"], true);
    let mut legacy = config();
    legacy.endpoint = Some("https://collector.example.com/v1/telemetry/batches".to_owned());
    legacy.export_protocol = ExportProtocol::LegacyJson;
    let sdk = Telemetry::init(legacy).unwrap();
    assert_eq!(sdk.capabilities()["export_protocol"], "legacy_json");
}

struct PartialDeliveryTransport {
    batches: Mutex<Vec<Vec<String>>>,
}

impl Transport for PartialDeliveryTransport {
    fn send(&self, _: &[Event], _: Duration) -> Result<(), TransportError> {
        panic!("the worker must use send_report")
    }

    fn send_report(&self, events: &[Event], _: Duration) -> DeliveryReport {
        let mut batches = self.batches.lock().unwrap();
        batches.push(events.iter().map(|event| event.event_id.clone()).collect());
        if batches.len() == 1 {
            DeliveryReport {
                consumed: vec![events[0].event_id.clone()],
                rejected: 0,
                error: Some(TransportError {
                    message: "later signal temporarily unavailable".to_owned(),
                    retryable: true,
                    retry_after: None,
                }),
            }
        } else {
            DeliveryReport {
                consumed: events.iter().map(|event| event.event_id.clone()).collect(),
                ..DeliveryReport::default()
            }
        }
    }
}

#[test]
fn successful_signal_is_not_replayed_after_a_later_signal_fails() {
    let transport = Arc::new(PartialDeliveryTransport {
        batches: Mutex::new(Vec::new()),
    });
    let sdk = Telemetry::with_transport(enabled_config(), transport.clone()).unwrap();
    sdk.action("first", ActionResult::Success).unwrap();
    sdk.increment("second", 1.0).unwrap();
    assert_eq!(
        sdk.flush(Duration::from_millis(30)),
        Err(Error::DeadlineExceeded)
    );
    assert_eq!(sdk.diagnostics().queue_depth, 1);
    assert_eq!(sdk.diagnostics().dropped, 0);
    // Advance retry eligibility rather than sleeping through production backoff.
    sdk.state().unwrap().retry_at = Instant::now();
    sdk.flush(Duration::from_secs(1)).unwrap();
    let batches = transport.batches.lock().unwrap();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 2);
    assert_eq!(batches[1], vec![batches[0][1].clone()]);
    assert_eq!(sdk.diagnostics().queue_depth, 0);
}

struct RejectedDeliveryTransport {
    calls: AtomicUsize,
}

impl Transport for RejectedDeliveryTransport {
    fn send(&self, _: &[Event], _: Duration) -> Result<(), TransportError> {
        panic!("the worker must use send_report")
    }

    fn send_report(&self, events: &[Event], _: Duration) -> DeliveryReport {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        DeliveryReport {
            consumed: events.iter().map(|event| event.event_id.clone()).collect(),
            rejected: usize::from(first),
            // A transport cannot suppress rejection by omitting the error.
            error: None,
        }
    }
}

#[test]
fn partial_rejection_is_consumed_without_replay_but_flush_reports_failure() {
    let transport = Arc::new(RejectedDeliveryTransport {
        calls: AtomicUsize::new(0),
    });
    let sdk = Telemetry::with_transport(enabled_config(), transport.clone()).unwrap();
    sdk.action("first", ActionResult::Success).unwrap();
    sdk.increment("second", 1.0).unwrap();
    assert_eq!(
        sdk.flush(Duration::from_secs(1)),
        Err(Error::Transport { retryable: false })
    );
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    assert_eq!(sdk.diagnostics().dropped, 1);
    assert_eq!(sdk.diagnostics().exporter_failures, 1);
    sdk.flush(Duration::ZERO).unwrap();
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    sdk.action("unrelated", ActionResult::Success).unwrap();
    sdk.flush(Duration::from_millis(100)).unwrap();
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    // Successful later flushes must not erase rejection history before an
    // earlier concurrent flush waiter has had a chance to observe it.
    assert_eq!(sdk.state().unwrap().last_permanent_error_attempt, 1);
    assert_eq!(sdk.state().unwrap().send_attempt, 2);
}

struct InvalidDeliveryTransport {
    mode: u8,
    calls: AtomicUsize,
}

impl Transport for InvalidDeliveryTransport {
    fn send(&self, _: &[Event], _: Duration) -> Result<(), TransportError> {
        panic!("the worker must use send_report")
    }

    fn send_report(&self, events: &[Event], _: Duration) -> DeliveryReport {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            0 => DeliveryReport {
                consumed: vec!["unrelated-record".to_owned()],
                ..DeliveryReport::default()
            },
            1 => DeliveryReport {
                consumed: vec![events[0].event_id.clone(); 2],
                ..DeliveryReport::default()
            },
            2 => DeliveryReport {
                consumed: vec![events[0].event_id.clone()],
                rejected: 2,
                error: None,
            },
            _ => DeliveryReport::default(),
        }
    }
}

#[test]
fn malformed_delivery_reports_cannot_remove_rows_or_spin_the_exporter() {
    for mode in 0..4 {
        let transport = Arc::new(InvalidDeliveryTransport {
            mode,
            calls: AtomicUsize::new(0),
        });
        let sdk = Telemetry::with_transport(enabled_config(), transport.clone()).unwrap();
        sdk.action("kept", ActionResult::Success).unwrap();
        assert_eq!(
            sdk.flush(Duration::from_secs(1)),
            Err(Error::Transport { retryable: false })
        );
        assert_eq!(sdk.diagnostics().queue_depth, 1);
        assert_eq!(sdk.diagnostics().dropped, 0);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}

#[cfg(feature = "sqlite")]
#[test]
fn persisted_queue_is_purged_when_app_identity_or_consent_changes() {
    let path = std::env::temp_dir().join(format!("telemetry-core-{}.sqlite", new_id()));
    let mut first = enabled_config();
    first.sqlite_path = Some(path.to_string_lossy().into_owned());
    let sdk = Telemetry::init(first).unwrap();
    sdk.action("search", ActionResult::Success).unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 1);
    drop(sdk);
    let mut second = enabled_config();
    second.app_id = "different.app".to_owned();
    second.sqlite_path = Some(path.to_string_lossy().into_owned());
    let sdk = Telemetry::init(second).unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    sdk.action("new", ActionResult::Success).unwrap();
    drop(sdk);
    let mut denied = config();
    denied.app_id = "different.app".to_owned();
    denied.sqlite_path = Some(path.to_string_lossy().into_owned());
    let sdk = Telemetry::init(denied).unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    drop(sdk);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
