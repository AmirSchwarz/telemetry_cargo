#![cfg(feature = "http")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telemetry::{ActionResult, Category, Config, Telemetry};
use telemetry_transport::HttpTransport;

#[test]
fn application_event_reaches_gateway_and_acknowledges_the_queue() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!(
        "http://{}/v1/telemetry/batches",
        listener.local_addr().unwrap()
    );
    let gateway = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "gateway request was not received"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("gateway accept failed: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let (body_offset, length) = loop {
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0, "truncated request headers");
            request.extend_from_slice(&chunk[..count]);
            assert!(request.len() < 65536);
            if let Some(end) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..end])
                    .unwrap()
                    .to_ascii_lowercase();
                assert!(headers.starts_with("post /v1/telemetry/batches http/1.1\r\n"));
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                break (end + 4, length);
            }
        };
        while request.len() < body_offset + length {
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0, "truncated batch");
            request.extend_from_slice(&chunk[..count]);
        }
        let batch: serde_json::Value =
            serde_json::from_slice(&request[body_offset..body_offset + length]).unwrap();
        stream
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        batch
    });

    let transport = HttpTransport::for_loopback_test(&endpoint, None).unwrap();
    let mut config = Config::new("gateway.example", "1.0.0");
    config.flush_interval_ms = 60000;
    let sdk = Telemetry::with_transport(config, Arc::new(transport)).unwrap();
    sdk.set_consent(Category::UsageAggregate, true).unwrap();
    sdk.action("search.submit", ActionResult::Success).unwrap();
    sdk.flush(Duration::from_secs(3)).unwrap();
    assert_eq!(sdk.diagnostics().queue_depth, 0);
    let batch = gateway.join().unwrap();
    assert_eq!(batch["schema_version"], 1);
    assert_eq!(batch["events"][0]["app_id"], "gateway.example");
    assert_eq!(batch["events"][0]["event_name"], "action");
    assert_eq!(batch["events"][0]["properties"]["id"], "search.submit");
    sdk.shutdown(Duration::from_secs(1)).unwrap();
}
