use super::{Error, Inner};
use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use telemetry_schema::{now_unix_ms, Event};
use telemetry_transport::{DeliveryReport, TransportError};

pub(super) fn start(inner: &Arc<Inner>) -> Result<(), Error> {
    let weak = Arc::downgrade(inner);
    std::thread::Builder::new()
        .name("telemetry-export".to_owned())
        .spawn(move || run(weak))
        .map(|_| ())
        .map_err(|_| Error::InvalidConfig("cannot start telemetry exporter"))
}

fn run(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let mut state = match inner.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if state.stopped {
            inner.changed.notify_all();
            return;
        }
        let now = Instant::now();
        let ready = (state.requested || now >= state.next_export) && now >= state.retry_at;
        if !ready {
            let next = if state.requested {
                state.retry_at
            } else {
                state.next_export.max(state.retry_at)
            };
            let wait = next
                .saturating_duration_since(now)
                .min(Duration::from_secs(60));
            let result = inner.changed.wait_timeout(state, wait);
            if result.is_err() {
                return;
            }
            // Drop this upgrade before the next iteration; the worker owns no client handle.
            continue;
        }
        drop(state);
        let upload_guard = match inner.upload_gate.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        state = match inner.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if state.stopped {
            inner.changed.notify_all();
            return;
        }
        let now = Instant::now();
        // Consent may have changed while we waited for the upload gate.
        let allowed = state.gate.allowed_categories();
        if state.queue.retain_categories(&allowed).is_err() {
            state.stopped = true;
            state.closing = true;
            inner.changed.notify_all();
            return;
        }
        let batch_size = state.config.batch_size;
        let before_batch = state.queue.len();
        let mut events = match state.queue.batch(batch_size, now_unix_ms()) {
            Ok(events) => events,
            Err(_) => {
                state.stopped = true;
                state.closing = true;
                inner.changed.notify_all();
                return;
            }
        };
        state.diagnostics.dropped = state
            .diagnostics
            .dropped
            .saturating_add(before_batch.saturating_sub(state.queue.len()) as u64);
        // A shared or tampered persistence file cannot cross application/schema boundaries.
        let invalid: Vec<String> = events
            .iter()
            .filter(|event| {
                event.app_id != state.config.app_id
                    || state
                        .registry
                        .validate(event, &state.config.limits)
                        .is_err()
            })
            .map(|event| event.event_id.clone())
            .collect();
        if !invalid.is_empty() {
            if state.queue.acknowledge(&invalid).is_err() {
                state.stopped = true;
                state.closing = true;
                inner.changed.notify_all();
                return;
            }
            events.retain(|event| !invalid.contains(&event.event_id));
            state.diagnostics.dropped = state
                .diagnostics
                .dropped
                .saturating_add(invalid.len() as u64);
        }
        if events.is_empty() {
            state.requested = !state.queue.is_empty();
            if state.queue.is_empty() {
                state.flush_deadline = None;
                state.next_export = now + Duration::from_millis(state.config.flush_interval_ms);
            } else {
                // A rejected first page must not strand valid later rows until the next interval.
                state.next_export = now;
            }
            state.failures = 0;
            inner.changed.notify_all();
            drop(state);
            drop(upload_guard);
            continue;
        }
        let mut timeout = Duration::from_secs(10);
        if let Some(deadline) = state.flush_deadline {
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                state.flush_deadline = None;
                state.requested = false;
                state.next_export = now + Duration::from_millis(state.config.flush_interval_ms);
                inner.changed.notify_all();
                drop(state);
                drop(upload_guard);
                continue;
            }
            timeout = timeout.min(remaining);
        }
        drop(state);
        // No state mutex is held across transport work. Collection stays responsive.
        let report = catch_unwind(AssertUnwindSafe(|| {
            inner
                .transport
                .as_ref()
                .expect("worker requires transport")
                .send_report(&events, timeout)
        }))
        .unwrap_or_else(|_| DeliveryReport {
            error: Some(TransportError {
                message: "exporter failed".to_owned(),
                retryable: false,
                retry_after: None,
            }),
            ..DeliveryReport::default()
        });
        let report = validate_report(report, &events);
        state = match inner.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        state.send_attempt = state.send_attempt.saturating_add(1);
        // Acknowledgements are per completed signal, even if a later signal fails.
        // Definitive OTLP rejections are consumed too, and never replayed.
        if state.queue.acknowledge(&report.consumed).is_err() {
            state.stopped = true;
            state.closing = true;
            inner.changed.notify_all();
            return;
        }
        state.diagnostics.dropped = state
            .diagnostics
            .dropped
            .saturating_add(report.rejected as u64);
        match report.error {
            None => {
                state.failures = 0;
                state.retry_at = Instant::now();
                state.requested = !state.queue.is_empty();
                if state.queue.is_empty() {
                    state.flush_deadline = None;
                }
                state.next_export =
                    Instant::now() + Duration::from_millis(state.config.flush_interval_ms);
            }
            Some(error) => {
                state.diagnostics.exporter_failures =
                    state.diagnostics.exporter_failures.saturating_add(1);
                state.failures = state.failures.saturating_add(1);
                if !error.retryable {
                    // Keep failure history across subsequent successful sends:
                    // another flush can otherwise hide rejection from a waiter.
                    state.last_permanent_error_attempt = state.send_attempt;
                }
                let jitter = Duration::from_millis((now_unix_ms() ^ state.send_attempt) % 251);
                let exponential =
                    Duration::from_secs(1_u64 << state.failures.saturating_sub(1).min(6));
                let delay = if error.retryable {
                    error
                        .retry_after
                        .unwrap_or_default()
                        .max(exponential + jitter)
                } else {
                    Duration::from_millis(state.config.flush_interval_ms)
                        .max(Duration::from_secs(60))
                };
                // After TTL there is no live payload left to retry; oversized server delays
                // therefore need not keep a worker alive beyond the queue lifetime.
                let delay = delay.min(Duration::from_millis(state.config.queue.ttl_ms));
                state.retry_at = Instant::now()
                    .checked_add(delay)
                    .unwrap_or(Instant::now() + Duration::from_secs(86_400));
                state.requested = error.retryable;
                state.next_export = state.retry_at;
                if !error.retryable {
                    state.flush_deadline = None;
                    if state.queue.is_empty() {
                        // No rejected payload remains to retry. Do not penalize
                        // unrelated records collected after this completed batch.
                        state.retry_at = Instant::now();
                        state.next_export =
                            state.retry_at + Duration::from_millis(state.config.flush_interval_ms);
                    }
                }
            }
        }
        inner.changed.notify_all();
        drop(state);
        drop(upload_guard);
    }
}

fn validate_report(mut report: DeliveryReport, events: &[Event]) -> DeliveryReport {
    let expected: BTreeSet<&str> = events.iter().map(|event| event.event_id.as_str()).collect();
    let mut unique = BTreeSet::new();
    let valid_ids = report
        .consumed
        .iter()
        .all(|id| expected.contains(id.as_str()) && unique.insert(id.as_str()));
    if !valid_ids
        || report.rejected > report.consumed.len()
        || (report.error.is_none() && report.consumed.is_empty() && !events.is_empty())
    {
        return DeliveryReport {
            error: Some(TransportError {
                message: "invalid exporter delivery report".to_owned(),
                retryable: false,
                retry_after: None,
            }),
            ..DeliveryReport::default()
        };
    }
    if report.rejected > 0 {
        // A transport cannot hide definitive losses behind an otherwise successful
        // report, or ask the worker to replay a partially rejected request.
        report.error = Some(TransportError {
            message: "telemetry records rejected".to_owned(),
            retryable: false,
            retry_after: None,
        });
    }
    report
}
