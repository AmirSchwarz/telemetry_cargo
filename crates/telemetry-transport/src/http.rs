use crate::{Transport, TransportError, MAX_BATCH_BYTES, MAX_REQUEST_TIMEOUT};
use reqwest::{
    blocking::Client,
    header::{HeaderValue, AUTHORIZATION, RETRY_AFTER},
    redirect::Policy,
    Url,
};
use serde::Serialize;
use std::{fmt, io::Write, net::IpAddr, sync::OnceLock, time::Duration};
use telemetry_schema::Event;

/// HTTPS batch transport. Redirects, proxy environment settings, URL credentials,
/// fragments, and query strings are disabled. Tokens are public ingestion tokens,
/// never administration credentials. Debug output deliberately omits configuration.
pub struct HttpTransport {
    client: OnceLock<Result<Client, TransportError>>,
    endpoint: Url,
    authorization: Option<HeaderValue>,
    allow_loopback_http: bool,
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTransport")
            .finish_non_exhaustive()
    }
}

impl HttpTransport {
    pub fn new(endpoint: &str, token: Option<String>) -> Result<Self, TransportError> {
        Self::build(endpoint, token, false)
    }

    /// Explicit plaintext exception for local synthetic integration tests.
    /// Only literal loopback IP addresses are accepted; `localhost` is not.
    /// Never use this constructor for production application configuration.
    pub fn for_loopback_test(
        endpoint: &str,
        token: Option<String>,
    ) -> Result<Self, TransportError> {
        Self::build(endpoint, token, true)
    }

    fn build(
        endpoint: &str,
        token: Option<String>,
        allow_loopback_http: bool,
    ) -> Result<Self, TransportError> {
        let authority_has_userinfo = endpoint
            .split_once("://")
            .and_then(|(_, authority_and_path)| authority_and_path.split('/').next())
            .is_some_and(|authority| authority.contains('@'));
        if endpoint.len() > 2048
            || endpoint
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
            || authority_has_userinfo
        {
            return Err(TransportError::permanent("invalid telemetry endpoint"));
        }
        let endpoint = Url::parse(endpoint)
            .map_err(|_| TransportError::permanent("invalid telemetry endpoint"))?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|address| match address {
                IpAddr::V4(ip) => ip.octets() == [127, 0, 0, 1],
                IpAddr::V6(ip) => ip.is_loopback(),
            });
        let valid_scheme = if allow_loopback_http {
            endpoint.scheme() == "http" && loopback
        } else {
            endpoint.scheme() == "https"
        };
        if !valid_scheme
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || endpoint.query().is_some()
            || endpoint.path() != "/v1/telemetry/batches"
        {
            return Err(TransportError::permanent("invalid telemetry endpoint"));
        }
        let authorization = token
            .map(|token| {
                if token.is_empty()
                    || token.len() > 2048
                    || !token.bytes().all(|byte| byte.is_ascii_graphic())
                {
                    return Err(TransportError::permanent("invalid ingestion token"));
                }
                let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| TransportError::permanent("invalid ingestion token"))?;
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;
        Ok(Self {
            client: OnceLock::new(),
            endpoint,
            authorization,
            allow_loopback_http,
        })
    }

    fn client(&self) -> Result<&Client, TransportError> {
        // reqwest's blocking client must be initialized outside async runtime
        // contexts. Construction happens here on the exporter worker, never in
        // the application's Telemetry::init call.
        self.client
            .get_or_init(|| {
                Client::builder()
                    .https_only(!self.allow_loopback_http)
                    .redirect(Policy::none())
                    .no_proxy()
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(MAX_REQUEST_TIMEOUT)
                    .pool_max_idle_per_host(1)
                    .user_agent(concat!("telemetry-rust/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .map_err(|_| TransportError::permanent("could not initialize HTTPS transport"))
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

/// Stop serialization at the budget rather than first allocating an oversized body.
struct BoundedBody(Vec<u8>);

impl Write for BoundedBody {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_BATCH_BYTES {
            return Err(std::io::Error::other("batch budget exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Transport for HttpTransport {
    fn send(&self, events: &[Event], timeout: Duration) -> Result<(), TransportError> {
        if events.is_empty() {
            return Ok(());
        }
        if timeout.is_zero() {
            return Err(TransportError {
                message: "telemetry request deadline expired".into(),
                retryable: true,
                retry_after: None,
            });
        }
        #[derive(Serialize)]
        struct Batch<'a> {
            schema_version: u32,
            events: &'a [Event],
        }
        let started = std::time::Instant::now();
        let timeout = timeout.min(MAX_REQUEST_TIMEOUT);
        let mut body = BoundedBody(Vec::new());
        serde_json::to_writer(
            &mut body,
            &Batch {
                schema_version: 1,
                events,
            },
        )
        .map_err(|_| TransportError::permanent("telemetry batch exceeds encoding budget"))?;
        let client = self.client()?;
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(TransportError {
                message: "telemetry request deadline expired".into(),
                retryable: true,
                retry_after: None,
            });
        }
        let mut request = client
            .post(self.endpoint.clone())
            .timeout(remaining)
            .header("content-type", "application/json")
            .body(body.0);
        if let Some(authorization) = &self.authorization {
            request = request.header(AUTHORIZATION, authorization.clone());
        }
        let response = request.send().map_err(|_| TransportError {
            message: "telemetry request failed".into(),
            retryable: true,
            retry_after: None,
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let retryable =
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
        let retry_after = if retryable {
            response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after)
        } else {
            None
        };
        // Do not read response bodies: they can contain secrets, malicious payloads,
        // or arbitrarily large data and do not belong in client diagnostics.
        Err(TransportError {
            message: format!("telemetry gateway returned HTTP {}", status.as_u16()),
            retryable,
            retry_after,
        })
    }
}

pub(super) fn parse_retry_after(value: &str) -> Option<Duration> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    parse_retry_after_at(value, now)
}

fn parse_retry_after_at(value: &str, now: u64) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse::<u64>().ok().map(Duration::from_secs);
    }
    // IMF-fixdate (the current HTTP-date format), without another dependency.
    // Obsolete RFC 850/asctime formats are deliberately unsupported.
    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.len() != 6
        || !["Mon,", "Tue,", "Wed,", "Thu,", "Fri,", "Sat,", "Sun,"].contains(&fields[0])
        || fields[1].len() != 2
        || fields[3].len() != 4
        || fields[5] != "GMT"
    {
        return None;
    }
    if !fields[1].bytes().all(|byte| byte.is_ascii_digit())
        || !fields[3].bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let day = fields[1].parse::<u32>().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|month| *month == fields[2])?;
    let year = fields[3].parse::<u32>().ok()?;
    if !(1601..=9999).contains(&year) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0 || day > month_lengths[month] {
        return None;
    }
    let time = fields[4].split(':').collect::<Vec<_>>();
    if time.len() != 3
        || time.iter().any(|component| {
            component.len() != 2 || !component.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return None;
    }
    let hour = time[0].parse::<u32>().ok()?;
    let minute = time[1].parse::<u32>().ok()?;
    let second = time[2].parse::<u32>().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let previous_year = i64::from(year) - 1;
    let leap_days = previous_year / 4 - previous_year / 100 + previous_year / 400;
    let days_before_year = 365 * previous_year + leap_days;
    // The Gregorian day count preceding 1970-01-01.
    let days = days_before_year - 719_162
        + i64::from(month_lengths[..month].iter().sum::<u32>())
        + i64::from(day - 1);
    let timestamp = days * 86_400 + i64::from(hour * 3600 + minute * 60 + second);
    let timestamp = u64::try_from(timestamp).unwrap_or_default();
    Some(Duration::from_secs(timestamp.saturating_sub(now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    fn event() -> Event {
        serde_json::from_value(serde_json::json!({
            "schema_version":1,
            "event_id":"00000000-0000-4000-8000-000000000001",
            "app_id":"test.app", "app_version":"1.0", "platform":"linux",
            "sdk_version":"0.1.0", "category":"diagnostics",
            "event_name":"error", "occurred_at":1,
            "properties":{"code":"test_error","message":"[redacted]"}
        }))
        .unwrap()
    }

    fn server(
        status: &str,
        headers: &str,
        delay: Duration,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/telemetry/batches",
            listener.local_addr().unwrap()
        );
        let response =
            format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n{headers}\r\n");
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let length = stream.read(&mut chunk).unwrap();
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..length]);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]);
                    let content_length = header
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + content_length {
                        break;
                    }
                }
            }
            let _ = sender.send(String::from_utf8(request).unwrap());
            thread::sleep(delay);
            let _ = stream.write_all(response.as_bytes());
        });
        (endpoint, receiver, worker)
    }

    #[test]
    fn only_explicit_https_contract_endpoint_accepted() {
        for endpoint in [
            "http://example.com/v1/telemetry/batches",
            "https://user:secret@example.com/v1/telemetry/batches",
            "https://@example.com/v1/telemetry/batches",
            "https://exam\nple.com/v1/telemetry/batches",
            "https://example.com/v1/telemetry/batches?secret=value",
            "https://example.com/v1/telemetry/batches#fragment",
            "https://example.com/elsewhere",
            "https:///",
        ] {
            let error = HttpTransport::new(endpoint, None).unwrap_err();
            assert_eq!(error.message, "invalid telemetry endpoint");
            assert!(!error.retryable);
        }
        assert!(HttpTransport::new("https://example.com/v1/telemetry/batches", None).is_ok());
        for endpoint in [
            "http://localhost/v1/telemetry/batches",
            "http://127.0.0.2/v1/telemetry/batches",
            "http://192.0.2.1/v1/telemetry/batches",
            "https://127.0.0.1/v1/telemetry/batches",
        ] {
            assert!(HttpTransport::for_loopback_test(endpoint, None).is_err());
        }
        assert!(
            HttpTransport::for_loopback_test("http://[::1]/v1/telemetry/batches", None).is_ok()
        );
    }

    #[test]
    fn tokens_are_validated_and_debug_output_is_private() {
        let endpoint = "https://example.com/v1/telemetry/batches";
        for token in ["", "has spaces", "secret\r\nx-injected: true", "é"] {
            let error = HttpTransport::new(endpoint, Some(token.into())).unwrap_err();
            assert_eq!(error.message, "invalid ingestion token");
            assert!(!error.message.contains(token) || token.is_empty());
        }
        let client = HttpTransport::new(endpoint, Some("example-public-token".into())).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("example-public-token"));
        assert!(!debug.contains("example.com"));
    }

    #[test]
    fn sends_envelope_and_bearer_header_and_acknowledges_success() {
        let (endpoint, request, worker) = server("202 Accepted", "", Duration::ZERO);
        let client =
            HttpTransport::for_loopback_test(&endpoint, Some("public-token".into())).unwrap();
        client.send(&[event()], Duration::from_secs(1)).unwrap();
        let request = request.recv().unwrap();
        assert!(request.starts_with("POST /v1/telemetry/batches HTTP/1.1\r\n"));
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer public-token"));
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["schema_version"], 1);
        assert_eq!(body["events"][0]["event_name"], "error");
        assert_eq!(body["events"].as_array().unwrap().len(), 1);
        worker.join().unwrap();
    }

    #[test]
    fn classifies_http_errors_and_retry_after() {
        for (status, retryable) in [
            ("400 Bad Request", false),
            ("401 Unauthorized", false),
            ("408 Request Timeout", true),
            ("429 Too Many Requests", true),
            ("503 Service Unavailable", true),
        ] {
            let (endpoint, _, worker) = server(status, "Retry-After: 7\r\n", Duration::ZERO);
            let client = HttpTransport::for_loopback_test(&endpoint, None).unwrap();
            let error = client.send(&[event()], Duration::from_secs(1)).unwrap_err();
            assert_eq!(error.retryable, retryable);
            assert_eq!(
                error.retry_after,
                retryable.then_some(Duration::from_secs(7))
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn redirects_are_never_followed() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let headers = format!(
            "Location: http://{}/v1/telemetry/batches\r\n",
            target.local_addr().unwrap()
        );
        let (endpoint, _, worker) = server("307 Temporary Redirect", &headers, Duration::ZERO);
        let client =
            HttpTransport::for_loopback_test(&endpoint, Some("public-token".into())).unwrap();
        let error = client.send(&[event()], Duration::from_secs(1)).unwrap_err();
        assert!(!error.retryable);
        assert_eq!(error.message, "telemetry gateway returned HTTP 307");
        assert!(target.accept().is_err());
        worker.join().unwrap();
    }

    #[test]
    fn request_deadline_is_enforced() {
        let (endpoint, _, worker) = server("200 OK", "", Duration::from_millis(250));
        let client = HttpTransport::for_loopback_test(&endpoint, None).unwrap();
        let started = Instant::now();
        let error = client
            .send(&[event()], Duration::from_millis(30))
            .unwrap_err();
        assert!(error.retryable);
        assert!(started.elapsed() < Duration::from_millis(200));
        worker.join().unwrap();
        let error = client.send(&[event()], Duration::ZERO).unwrap_err();
        assert_eq!(error.message, "telemetry request deadline expired");
        assert!(client.send(&[], Duration::ZERO).is_ok());
    }

    #[test]
    fn serialization_budget_is_bounded_before_network() {
        let client =
            HttpTransport::for_loopback_test("http://127.0.0.1:1/v1/telemetry/batches", None)
                .unwrap();
        let mut oversized = event();
        oversized.properties.insert(
            "message".into(),
            serde_json::Value::String("x".repeat(MAX_BATCH_BYTES + 1)),
        );
        let error = client
            .send(&[oversized], Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(error.message, "telemetry batch exceeds encoding budget");
        assert!(!error.retryable);
    }

    #[test]
    fn retry_after_supports_delta_seconds_and_imf_fixdate() {
        assert_eq!(parse_retry_after_at("7", 0), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after_at("+7", 0), None);
        assert_eq!(parse_retry_after_at("-1", 0), None);
        assert_eq!(
            parse_retry_after_at("Sun, 06 Nov 1994 08:49:37 GMT", 0),
            Some(Duration::from_secs(784_111_777))
        );
        assert_eq!(
            parse_retry_after_at("Thu, 01 Jan 1970 00:00:05 GMT", 3),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            parse_retry_after_at("Sun, 06 Nov 1994 08:49:37 GMT", 1_000_000_000),
            Some(Duration::ZERO)
        );
        for invalid in [
            "Sun, 31 Feb 2024 08:49:37 GMT",
            "Sun, 29 Feb 2023 08:49:37 GMT",
            "Sun, 06 Nov 1994 24:49:37 GMT",
            "Sun, 06 Nov 1994 08:60:37 GMT",
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sun, 06 Xxx 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
        ] {
            assert_eq!(parse_retry_after_at(invalid, 0), None, "{invalid}");
        }
    }
}
