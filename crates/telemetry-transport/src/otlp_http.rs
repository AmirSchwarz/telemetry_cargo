use crate::{
    otlp_requests, DeliveryReport, OtlpRequest, Transport, TransportError, MAX_BATCH_BYTES,
    MAX_REQUEST_TIMEOUT,
};
use reqwest::{
    blocking::{Client, Response},
    header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, RETRY_AFTER},
    redirect::Policy,
    Url,
};
use std::{
    collections::BTreeMap,
    fmt,
    io::{Read, Write},
    net::IpAddr,
    sync::OnceLock,
    time::{Duration, Instant},
};
use telemetry_schema::Event;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_HEADERS: usize = 16;
const MAX_HEADER_BYTES: usize = 8192;

/// OTLP/HTTP JSON exporter for logs, metrics and traces.
///
/// `endpoint` is a collector base URL. Signal-specific paths are appended to
/// this base. Credentials belong in the bounded header map or bearer token,
/// never in the URL. The plaintext exception requires an explicit opt-in and
/// an exact literal loopback address; production destinations use HTTPS.
pub struct OtlpHttpTransport {
    client: OnceLock<Result<Client, TransportError>>,
    endpoint: Url,
    headers: HeaderMap,
    allow_insecure_loopback: bool,
}

impl fmt::Debug for OtlpHttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OtlpHttpTransport")
            .finish_non_exhaustive()
    }
}

impl OtlpHttpTransport {
    pub fn new(
        endpoint: &str,
        token: Option<String>,
        headers: BTreeMap<String, String>,
        allow_insecure_loopback: bool,
    ) -> Result<Self, TransportError> {
        let endpoint = validate_endpoint(endpoint, allow_insecure_loopback)?;
        let headers = validate_headers(token, headers)?;
        Ok(Self {
            client: OnceLock::new(),
            endpoint,
            headers,
            allow_insecure_loopback,
        })
    }

    fn client(&self) -> Result<&Client, TransportError> {
        // Construction is lazy: a blocking Reqwest runtime must be created on
        // the exporter worker, not inside a host application's async runtime.
        self.client
            .get_or_init(|| {
                Client::builder()
                    .https_only(!self.allow_insecure_loopback)
                    .redirect(Policy::none())
                    .no_proxy()
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(MAX_REQUEST_TIMEOUT)
                    .pool_max_idle_per_host(1)
                    .user_agent(concat!("telemetry-cargo/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .map_err(|_| TransportError::permanent("could not initialize OTLP transport"))
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    fn send_request(
        &self,
        request: &OtlpRequest,
        body: Vec<u8>,
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        let client = self.client()?;
        let mut url = self.endpoint.clone();
        url.set_path(&format!(
            "{}{}",
            self.endpoint.path().trim_end_matches('/'),
            request.path
        ));
        let response = client
            .post(url)
            .timeout(timeout)
            .headers(self.headers.clone())
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(body)
            .send()
            .map_err(|_| transient("OTLP request failed"))?;
        if response.status().as_u16() != 200 {
            let status = response.status().as_u16();
            let retryable = matches!(status, 429 | 502 | 503 | 504);
            let retry_after = if retryable {
                response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(super::http::parse_retry_after)
            } else {
                None
            };
            return Err(TransportError {
                message: format!("OTLP collector returned HTTP {status}"),
                retryable,
                retry_after,
            });
        }
        read_response(response, request)
    }
}

impl Transport for OtlpHttpTransport {
    fn send(&self, events: &[Event], timeout: Duration) -> Result<(), TransportError> {
        self.send_report(events, timeout).error.map_or(Ok(()), Err)
    }

    fn send_report(&self, events: &[Event], timeout: Duration) -> DeliveryReport {
        if events.is_empty() {
            return DeliveryReport::default();
        }
        let started = Instant::now();
        let timeout = timeout.min(MAX_REQUEST_TIMEOUT);
        if timeout.is_zero() {
            return failed_report(transient("OTLP export deadline expired"));
        }
        let requests = match otlp_requests(events) {
            Ok(requests) => requests,
            Err(error) => return reject_all(events, error),
        };
        // Check and serialize the entire mixed-signal attempt before networking,
        // so an oversized later signal cannot cause a partly sent request set.
        let mut remaining_budget = MAX_BATCH_BYTES;
        let mut encoded = Vec::with_capacity(requests.len());
        for request in requests {
            let mut body = BoundedBody {
                bytes: Vec::new(),
                limit: remaining_budget,
            };
            if serde_json::to_writer(&mut body, &request.body).is_err() {
                return reject_all(
                    events,
                    TransportError::permanent("OTLP batch exceeds encoding budget"),
                );
            }
            remaining_budget -= body.bytes.len();
            encoded.push((request, body.bytes));
        }
        // Include lazy runtime/client construction in the shared deadline.
        if let Err(error) = self.client() {
            return failed_report(error);
        }
        let mut report = DeliveryReport::default();
        for (request, body) in encoded {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                record_error(&mut report, transient("OTLP export deadline expired"));
                break;
            }
            match self.send_request(&request, body, remaining) {
                Ok(rejected) => {
                    report.consumed.extend(request.event_ids);
                    report.rejected += rejected;
                    if rejected > 0 {
                        record_error(
                            &mut report,
                            TransportError::permanent("OTLP collector partially rejected records"),
                        );
                    }
                }
                Err(error) if error.retryable => {
                    record_error(&mut report, error);
                    break;
                }
                Err(error) => {
                    // OTLP forbids replay after definitive rejection or partial
                    // success. The other signals may still be accepted.
                    report.rejected += request.event_ids.len();
                    report.consumed.extend(request.event_ids);
                    record_error(&mut report, error);
                }
            }
        }
        report
    }
}

fn validate_endpoint(endpoint: &str, allow_insecure_loopback: bool) -> Result<Url, TransportError> {
    let invalid = || TransportError::permanent("invalid OTLP collector endpoint");
    if endpoint.is_empty()
        || endpoint.len() > 2048
        || endpoint
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b' ' | b'\\' | b'%'))
    {
        return Err(invalid());
    }
    let (_, rest) = endpoint.split_once("://").ok_or_else(invalid)?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(invalid());
    }
    let raw_path = rest
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or_default();
    if raw_path.split('/').any(|segment| {
        matches!(segment, "." | "..")
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    }) || raw_path.trim_end_matches('/').starts_with('/')
        || raw_path.trim_end_matches('/').contains("//")
    {
        return Err(invalid());
    }
    let url = Url::parse(endpoint).map_err(|_| invalid())?;
    let loopback = url
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|ip| match ip {
            IpAddr::V4(ip) => ip.octets() == [127, 0, 0, 1],
            IpAddr::V6(ip) => ip.is_loopback(),
        })
        && (authority == "127.0.0.1"
            || authority.starts_with("127.0.0.1:")
            || authority == "[::1]"
            || authority.starts_with("[::1]:"));
    let path = url.path().trim_end_matches('/');
    let signal_suffix = [
        "/v1",
        "/v1/logs",
        "/v1/metrics",
        "/v1/traces",
        "/v1/telemetry/batches",
    ]
    .iter()
    .any(|suffix| path.ends_with(suffix));
    if !(url.scheme() == "https" || (allow_insecure_loopback && url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || signal_suffix
    {
        return Err(invalid());
    }
    Ok(url)
}

fn validate_headers(
    token: Option<String>,
    headers: BTreeMap<String, String>,
) -> Result<HeaderMap, TransportError> {
    let invalid = || TransportError::permanent("invalid OTLP ingestion headers");
    if headers.len() > MAX_HEADERS {
        return Err(invalid());
    }
    let mut result = HeaderMap::new();
    let mut total_bytes = 0_usize;
    for (name, value) in headers {
        if name.is_empty()
            || name.len() > 64
            || !name.is_ascii()
            || value.is_empty()
            || value.len() > 2048
            || value.trim() != value
            || !value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
        {
            return Err(invalid());
        }
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        if forbidden_header(name.as_str()) || result.contains_key(&name) {
            return Err(invalid());
        }
        total_bytes = total_bytes.saturating_add(name.as_str().len() + value.len());
        let mut value = HeaderValue::from_str(&value).map_err(|_| invalid())?;
        value.set_sensitive(true);
        result.insert(name, value);
    }
    if let Some(token) = token {
        if token.is_empty()
            || token.len() > 2048
            || !token.bytes().all(|byte| byte.is_ascii_graphic())
            || result.contains_key(AUTHORIZATION)
        {
            return Err(invalid());
        }
        total_bytes =
            total_bytes.saturating_add("authorization".len() + "Bearer ".len() + token.len());
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| invalid())?;
        value.set_sensitive(true);
        result.insert(AUTHORIZATION, value);
    }
    if result.len() > MAX_HEADERS || total_bytes > MAX_HEADER_BYTES {
        return Err(invalid());
    }
    Ok(result)
}

fn forbidden_header(name: &str) -> bool {
    name.starts_with("proxy-")
        || name.starts_with("sec-")
        || matches!(
            name,
            "host"
                | "content-length"
                | "content-type"
                | "content-encoding"
                | "accept"
                | "accept-encoding"
                | "connection"
                | "keep-alive"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "cookie"
                | "cookie2"
                | "set-cookie"
                | "set-cookie2"
                | "user-agent"
                | "origin"
                | "referer"
        )
}

fn read_response(response: Response, request: &OtlpRequest) -> Result<usize, TransportError> {
    let mut body = Vec::new();
    // Reqwest decompresses before Read, so this also bounds compressed bodies.
    response
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| transient("OTLP response could not be read"))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(TransportError::permanent(
            "OTLP response exceeds size limit",
        ));
    }
    parse_response(&body, request)
}

fn parse_response(body: &[u8], request: &OtlpRequest) -> Result<usize, TransportError> {
    crate::parse_otlp_response(body, request.signal, request.event_ids.len())
}

fn transient(message: &str) -> TransportError {
    TransportError {
        message: message.to_owned(),
        retryable: true,
        retry_after: None,
    }
}

fn failed_report(error: TransportError) -> DeliveryReport {
    DeliveryReport {
        error: Some(error),
        ..DeliveryReport::default()
    }
}

fn reject_all(events: &[Event], error: TransportError) -> DeliveryReport {
    DeliveryReport {
        consumed: events.iter().map(|event| event.event_id.clone()).collect(),
        rejected: events.len(),
        error: Some(error),
    }
}

fn record_error(report: &mut DeliveryReport, error: TransportError) {
    if report
        .error
        .as_ref()
        .is_none_or(|old| old.retryable && !error.retryable)
    {
        report.error = Some(error);
    }
}

struct BoundedBody {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for BoundedBody {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(std::io::Error::other("OTLP encoding budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{
        net::TcpListener,
        sync::mpsc::{self, Receiver},
        thread::{self, JoinHandle},
    };

    fn event(id: u8, name: &str, properties: Value) -> Event {
        let category = match name {
            "span" => "performance",
            "metric.increment" | "action" => "usage_aggregate",
            _ => "diagnostics",
        };
        serde_json::from_value(json!({
            "schema_version":1,
            "event_id":format!("00000000-0000-4000-8000-{id:012x}"),
            "app_id":"otlp.test", "app_version":"1.0", "platform":"linux",
            "sdk_version":"0.1.0", "category":category,
            "event_name":name, "occurred_at":1000,
            "properties":properties
        }))
        .unwrap()
    }

    fn error_event(id: u8) -> Event {
        event(
            id,
            "error",
            json!({"code":"test.failure","message":"[redacted]"}),
        )
    }

    fn metric_event(id: u8) -> Event {
        event(
            id,
            "metric.increment",
            json!({"name":"files.imported","amount":3}),
        )
    }

    fn span_event(id: u8) -> Event {
        event(id, "span", json!({"name":"document.ocr","duration_ms":10}))
    }

    struct Reply {
        status: u16,
        body: String,
        extra: String,
        delay: Duration,
    }

    impl Reply {
        fn ok(body: &str) -> Self {
            Self {
                status: 200,
                body: body.to_owned(),
                extra: String::new(),
                delay: Duration::ZERO,
            }
        }
    }

    fn server(replies: Vec<Reply>) -> (String, Receiver<String>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            for reply in replies {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return;
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let required = loop {
                    let mut chunk = [0; 2048];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    assert!(request.len() <= MAX_BATCH_BYTES + 32768);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length: usize = header
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        break end + 4 + length;
                    }
                };
                while request.len() < required {
                    let mut chunk = [0; 2048];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                }
                let _ = sender.send(String::from_utf8(request).unwrap());
                thread::sleep(reply.delay);
                let response = format!(
                    "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    reply.status, reply.body.len(), reply.extra, reply.body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (base, receiver, handle)
    }

    fn transport(base: &str) -> OtlpHttpTransport {
        OtlpHttpTransport::new(base, None, BTreeMap::new(), true).unwrap()
    }

    #[test]
    fn endpoints_are_bases_and_plaintext_is_explicit_literal_loopback_only() {
        for endpoint in [
            "http://127.0.0.1:4318",
            "http://localhost:4318",
            "http://collector.example.com",
            "https://user:secret@collector.example.com",
            "https://collector.example.com?token=secret",
            "https://collector.example.com#fragment",
            "https://collector.example.com/v1/logs",
            "https://collector.example.com/otlp/v1/traces/",
            "https://collector.example.com/v1/metrics",
            "https://collector.example.com/v1",
            "https://collector.example.com/a/../b",
            "https://collector.example.com/a//b",
            "https://collector.example.com//a",
            "https://collector.example.com/%2e%2e/b",
            "https://collector.example.com\\private",
        ] {
            assert!(OtlpHttpTransport::new(endpoint, None, BTreeMap::new(), false).is_err());
        }
        for endpoint in [
            "http://localhost:4318",
            "http://127.1:4318",
            "http://127.0.0.2:4318",
        ] {
            assert!(OtlpHttpTransport::new(endpoint, None, BTreeMap::new(), true).is_err());
        }
        for endpoint in [
            "https://collector.example.com",
            "https://collector.example.com/otlp/",
            "http://127.0.0.1:4318",
            "http://[::1]:4318",
        ] {
            assert!(OtlpHttpTransport::new(endpoint, None, BTreeMap::new(), true).is_ok());
        }
    }

    #[test]
    fn headers_are_bounded_and_debug_errors_do_not_echo_credentials() {
        let endpoint = "https://private.example.com/private-prefix";
        let transport = OtlpHttpTransport::new(
            endpoint,
            None,
            BTreeMap::from([("authorization".into(), "private-ingestion-key".into())]),
            false,
        )
        .unwrap();
        let debug = format!("{transport:?}");
        assert!(!debug.contains("private"));
        for (name, value) in [
            ("host", "example.com"),
            ("Content-Type", "text/plain"),
            ("Cookie", "private=value"),
            ("Proxy-Authorization", "secret"),
            ("Sec-Fetch-Site", "same-origin"),
            ("x-api-key", "secret\r\nInjected: true"),
            ("x-api-key", " secret"),
            ("x-api-key", ""),
        ] {
            let error = OtlpHttpTransport::new(
                endpoint,
                None,
                BTreeMap::from([(name.into(), value.into())]),
                false,
            )
            .unwrap_err();
            assert_eq!(error.message, "invalid OTLP ingestion headers");
        }
        assert!(OtlpHttpTransport::new(
            endpoint,
            Some("token".into()),
            BTreeMap::from([("AUTHORIZATION".into(), "raw-key".into())]),
            false
        )
        .is_err());
        assert!(OtlpHttpTransport::new(
            endpoint,
            None,
            BTreeMap::from([
                ("Authorization".into(), "a".into()),
                ("authorization".into(), "b".into())
            ]),
            false
        )
        .is_err());
        let many = (0..16)
            .map(|n| (format!("x-test-{n}"), "value".into()))
            .collect();
        assert!(OtlpHttpTransport::new(endpoint, Some("token".into()), many, false).is_err());
        let oversized = (0..5)
            .map(|n| (format!("x-test-{n}"), "x".repeat(2048)))
            .collect();
        assert!(OtlpHttpTransport::new(endpoint, None, oversized, false).is_err());
    }

    #[test]
    fn sends_three_standard_paths_with_raw_clickstack_auth_and_prefix() {
        let (base, requests, handle) =
            server(vec![Reply::ok("{}"), Reply::ok("{}"), Reply::ok("{}")]);
        let exporter = OtlpHttpTransport::new(
            &format!("{base}/otlp/"),
            None,
            BTreeMap::from([("authorization".into(), "public-ingestion-key".into())]),
            true,
        )
        .unwrap();
        let events = [error_event(1), metric_event(2), span_event(3)];
        let report = exporter.send_report(&events, Duration::from_secs(2));
        assert_eq!(report.consumed.len(), 3);
        assert_eq!(report.rejected, 0);
        assert!(report.error.is_none());
        for path in ["/otlp/v1/logs", "/otlp/v1/metrics", "/otlp/v1/traces"] {
            let request = requests.recv().unwrap();
            assert!(request.starts_with(&format!("POST {path} HTTP/1.1\r\n")));
            assert!(request.contains("authorization: public-ingestion-key\r\n"));
            assert!(request.contains("content-type: application/json\r\n"));
            assert!(!request.contains("Bearer public-ingestion-key"));
        }
        handle.join().unwrap();
    }

    #[test]
    fn consumes_only_successful_signals_before_transient_failure() {
        let mut retry = Reply::ok("{}");
        retry.status = 503;
        retry.extra = "Retry-After: 7\r\n".into();
        let (base, requests, handle) = server(vec![Reply::ok("{}"), retry]);
        let exporter =
            OtlpHttpTransport::new(&base, Some("public-token".into()), BTreeMap::new(), true)
                .unwrap();
        let events = [error_event(1), metric_event(2), span_event(3)];
        let report = exporter.send_report(&events, Duration::from_secs(2));
        assert_eq!(report.consumed, vec![events[0].event_id.clone()]);
        assert_eq!(report.rejected, 0);
        let error = report.error.unwrap();
        assert!(error.retryable);
        assert_eq!(error.retry_after, Some(Duration::from_secs(7)));
        assert!(requests
            .recv()
            .unwrap()
            .contains("authorization: Bearer public-token\r\n"));
        assert!(requests.recv().unwrap().starts_with("POST /v1/metrics"));
        handle.join().unwrap();
    }

    #[test]
    fn partial_success_is_consumed_without_retry_and_other_signals_continue() {
        let (base, _, handle) = server(vec![
            Reply::ok(
                r#"{"partialSuccess":{"rejectedLogRecords":"1","errorMessage":"do not leak this secret"}}"#,
            ),
            Reply::ok("{}"),
        ]);
        let events = [error_event(1), metric_event(2)];
        let report = transport(&base).send_report(&events, Duration::from_secs(2));
        assert_eq!(report.consumed.len(), 2);
        assert_eq!(report.rejected, 1);
        let error = report.error.unwrap();
        assert!(!error.retryable);
        assert!(!error.message.contains("secret"));
        handle.join().unwrap();
    }

    #[test]
    fn retryable_statuses_follow_otlp_and_redirects_are_definitive() {
        for status in [400, 401, 408, 429, 500, 502, 503, 504, 202, 307] {
            let mut reply = Reply::ok("private response body");
            reply.status = status;
            reply.extra = "Location: https://secret.invalid\r\n".into();
            let (base, _, handle) = server(vec![reply]);
            let event = error_event(1);
            let report = transport(&base).send_report(&[event], Duration::from_secs(2));
            let error = report.error.unwrap();
            let retryable = matches!(status, 429 | 502 | 503 | 504);
            assert_eq!(error.retryable, retryable, "status {status}");
            assert_eq!(report.consumed.len(), usize::from(!retryable));
            assert_eq!(report.rejected, usize::from(!retryable));
            assert!(!error.message.contains("private"));
            handle.join().unwrap();
        }
    }

    #[test]
    fn invalid_or_oversized_success_response_is_not_a_silent_ack() {
        for body in [
            "".to_owned(),
            "[]".to_owned(),
            "not-json secret".to_owned(),
            r#"{"partialSuccess":true}"#.to_owned(),
            r#"{"partialSuccess":{"rejectedLogRecords":"2"}}"#.to_owned(),
            r#"{"partialSuccess":{"rejectedLogRecords":-1}}"#.to_owned(),
            format!(r#"{{"padding":"{}"}}"#, "x".repeat(MAX_RESPONSE_BYTES)),
        ] {
            let (base, _, handle) = server(vec![Reply::ok(&body)]);
            let report = transport(&base).send_report(&[error_event(1)], Duration::from_secs(2));
            assert_eq!(report.consumed.len(), 1);
            assert_eq!(report.rejected, 1);
            let error = report.error.unwrap();
            assert!(!error.retryable);
            assert!(!error.message.contains("secret"));
            handle.join().unwrap();
        }
    }

    #[test]
    fn zero_rejection_warnings_and_unknown_response_fields_are_accepted() {
        for body in [
            r#"{"extra":"ignored"}"#,
            r#"{"partialSuccess":{}}"#,
            r#"{"partialSuccess":{"rejectedLogRecords":"0","errorMessage":"warning"}}"#,
            r#"{"partialSuccess":null}"#,
        ] {
            let (base, _, handle) = server(vec![Reply::ok(body)]);
            let report = transport(&base).send_report(&[error_event(1)], Duration::from_secs(2));
            assert_eq!(report.consumed.len(), 1);
            assert_eq!(report.rejected, 0);
            assert!(report.error.is_none());
            handle.join().unwrap();
        }
    }

    #[test]
    fn all_signals_share_one_export_deadline() {
        let mut first = Reply::ok("{}");
        first.delay = Duration::from_millis(100);
        let mut second = Reply::ok("{}");
        second.delay = Duration::from_millis(100);
        let (base, _, handle) = server(vec![first, second]);
        let started = Instant::now();
        let report = transport(&base).send_report(
            &[error_event(1), metric_event(2)],
            Duration::from_millis(150),
        );
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(report.consumed.len(), 1);
        assert!(report.error.unwrap().retryable);
        handle.join().unwrap();
    }

    #[test]
    fn response_rejection_fields_match_each_signal_and_zero_timeout_keeps_events() {
        for (event, field) in [
            (error_event(1), "rejectedLogRecords"),
            (metric_event(2), "rejectedDataPoints"),
            (span_event(3), "rejectedSpans"),
        ] {
            let body = serde_json::to_string(&json!({"partialSuccess":{field:1}})).unwrap();
            let (base, _, handle) = server(vec![Reply::ok(&body)]);
            let report = transport(&base).send_report(&[event], Duration::from_secs(2));
            assert_eq!(report.consumed.len(), 1);
            assert_eq!(report.rejected, 1);
            assert!(!report.error.unwrap().retryable);
            handle.join().unwrap();
        }
        let report = transport("http://127.0.0.1:9").send_report(&[error_event(1)], Duration::ZERO);
        assert!(report.consumed.is_empty());
        assert_eq!(report.rejected, 0);
        assert!(report.error.unwrap().retryable);
    }

    #[test]
    fn protojson_partial_rejection_reports_the_exact_count_over_http() {
        for encoded in ["1.0", "1e0", r#""1e0""#, "null"] {
            let body = format!(
                r#"{{"partialSuccess":{{"rejectedLogRecords":{encoded},"errorMessage":null}}}}"#
            );
            let (base, _, handle) = server(vec![Reply::ok(&body)]);
            let report = transport(&base)
                .send_report(&[error_event(1), error_event(2)], Duration::from_secs(2));
            assert_eq!(report.consumed.len(), 2);
            assert_eq!(report.rejected, usize::from(encoded != "null"));
            assert_eq!(report.error.is_some(), encoded != "null");
            handle.join().unwrap();
        }
    }
}
