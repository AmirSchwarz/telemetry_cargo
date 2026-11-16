use std::time::Duration;
use telemetry::{ActionResult, Category, Config};

fn main() -> Result<(), telemetry::Error> {
    let mut config = Config::new("example.desktop", "1.0.0");
    // The endpoint is optional; offline events remain in the bounded queue.
    config.endpoint = std::env::var("TELEMETRY_ENDPOINT").ok();
    config.ingestion_token = std::env::var("TELEMETRY_INGESTION_TOKEN").ok();
    let sdk = telemetry::init(config)?;
    sdk.set_consent(Category::UsageAggregate, true)?;
    sdk.set_consent(Category::Diagnostics, true)?;
    sdk.set_consent(Category::Performance, true)?;
    sdk.action("search.submit", ActionResult::Success)?;
    sdk.increment("documents.imported", 3.0)?;
    sdk.capture_error("import.failed", "Raw messages are discarded by the SDK")?;
    {
        let _span = sdk.span("document.ocr")?;
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("{:?}", sdk.diagnostics());
    if std::env::var_os("TELEMETRY_ENDPOINT").is_some() {
        sdk.shutdown(Duration::from_secs(3))?;
    }
    Ok(())
}
