"""Integration against the actual PyO3 library, not a mock engine."""
import json
import logging
import telemetry
from telemetry._native import NativeTelemetry

sdk = telemetry.init({"app_id": "test.python", "app_version": "1.0.0"})
assert sdk.capabilities()["binding"] == "pyo3"
sdk.action("search.submit")
sdk.capture_error("error.test", "secret-token /private/name https://example.com")
assert sdk.diagnostics()["queue_depth"] == 0

sdk.set_consent("usage_aggregate", True)
sdk.action("search.submit")
sdk.increment("documents.imported", 3)
assert sdk.diagnostics()["queue_depth"] == 2

try:
    sdk.event("action", {"id": "search.submit", "result": "success", "query": "secret"})
except RuntimeError:
    pass
else:
    raise AssertionError("unknown properties accepted")
assert sdk.diagnostics()["queue_depth"] == 2

try:
    sdk.flush(10)
except RuntimeError:
    pass
else:
    raise AssertionError("offline flush silently discarded events")
assert sdk.diagnostics()["queue_depth"] == 2

sdk.set_consent("diagnostics", True)
handler = telemetry.TelemetryHandler(sdk)
handler.emit(logging.LogRecord("private-logger", logging.ERROR, "/private/document", 1, "secret-token", (), None))
assert sdk.diagnostics()["queue_depth"] == 3
sdk.set_consent("performance", True)

try:
    with sdk.span("invalid/path"):
        raise ValueError("application error")
except ValueError as error:
    assert str(error) == "application error"
else:
    raise AssertionError("application exception was swallowed")
try:
    with sdk.span("invalid/path"):
        pass
except RuntimeError:
    pass
else:
    raise AssertionError("invalid span should fail when the application succeeds")
with sdk.span("document.ocr"):
    pass
assert sdk.diagnostics()["queue_depth"] == 4
with sdk.span("document.ocr"):
    sdk.set_consent("performance", False)
    sdk.set_consent("performance", True)
# Old span must not cross a withdrawal/regrant epoch; prior span also purged.
assert sdk.diagnostics()["queue_depth"] == 3

for category in ("usage_aggregate", "diagnostics", "performance"):
    sdk.set_consent(category, False)
assert sdk.diagnostics()["queue_depth"] == 0
sdk.shutdown()
sdk.shutdown()  # Safe repeated close at Python wrapper boundary.

native = NativeTelemetry(json.dumps({"app_id": "test.native", "app_version": "1.0.0"}))
for invalid in ('{"op":"unknown"}', '{"op":"diagnostics","extra":true}', 'not json'):
    try:
        native.command(invalid)
    except (RuntimeError, ValueError):
        pass
    else:
        raise AssertionError("invalid command accepted")
native.close()

try:
    with telemetry.init({"app_id": "test.context", "app_version": "1.0.0", "consent": {"usage_aggregate": True}}) as context_sdk:
        context_sdk.action("search.submit")
        raise ValueError("application error during SDK context")
except ValueError as error:
    assert str(error) == "application error during SDK context"
else:
    raise AssertionError("SDK shutdown masked application exception")
print("Python native smoke passed")
