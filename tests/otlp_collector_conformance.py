"""Exercise the native SDK against an actual OTLP receiver and inspect its output.

Only synthetic application data is generated. Run scripts/check-otlp-collector.sh
to start the official collector and call this standard-library-only test.
"""

from __future__ import annotations

import argparse
import ctypes
import json
from pathlib import Path
import time
import urllib.error
import urllib.request


class Buffer(ctypes.Structure):
    _fields_ = [("data", ctypes.POINTER(ctypes.c_ubyte)), ("len", ctypes.c_size_t)]


class NativeSdk:
    def __init__(self, library: str, config: dict):
        self.lib = ctypes.CDLL(str(Path(library).resolve()))
        self.lib.telemetry_v1_init.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_uint64)]
        self.lib.telemetry_v1_init.restype = ctypes.c_int
        self.lib.telemetry_v1_command.argtypes = [ctypes.c_uint64, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(Buffer)]
        self.lib.telemetry_v1_command.restype = ctypes.c_int
        self.lib.telemetry_v1_buffer_free.argtypes = [Buffer]
        self.lib.telemetry_v1_buffer_free.restype = None
        self.lib.telemetry_v1_destroy.argtypes = [ctypes.c_uint64]
        self.lib.telemetry_v1_destroy.restype = ctypes.c_int
        self.handle = ctypes.c_uint64()
        encoded = json.dumps(config).encode()
        status = self.lib.telemetry_v1_init(encoded, len(encoded), ctypes.byref(self.handle))
        assert status == 0, f"SDK initialization failed with status {status}"

    def call(self, op: str, **fields):
        request = json.dumps({"op": op, **fields}).encode()
        output = Buffer()
        status = self.lib.telemetry_v1_command(self.handle.value, request, len(request), ctypes.byref(output))
        try:
            result = json.loads(ctypes.string_at(output.data, output.len)) if output.data else None
        finally:
            self.lib.telemetry_v1_buffer_free(output)
        assert status == 0, f"SDK operation {op} failed with status {status}"
        return result

    def destroy(self):
        if self.handle.value:
            status = self.lib.telemetry_v1_destroy(self.handle.value)
            self.handle.value = 0
            assert status == 0, f"SDK destruction failed with status {status}"


def wait_for_receiver(endpoint: str):
    deadline = time.monotonic() + 20
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    while time.monotonic() < deadline:
        try:
            with opener.open(endpoint + "/v1/logs", timeout=1):
                return
        except urllib.error.HTTPError as error:
            if error.code in (400, 404, 405, 415):
                return
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            pass
        time.sleep(0.1)
    raise AssertionError("collector did not become ready within 20 seconds")


def attributes(values: list) -> dict:
    return {item["key"]: next(iter(item["value"].values())) for item in values}


def inspect_output(path: Path):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            raw = path.read_text()
            batches = [json.loads(line) for line in raw.splitlines() if line.strip()]
            if any("resourceLogs" in item for item in batches) and any("resourceMetrics" in item for item in batches) and any("resourceSpans" in item for item in batches):
                break
        except (FileNotFoundError, json.JSONDecodeError):
            pass
        time.sleep(0.1)
    else:
        raise AssertionError("collector output did not contain all three OTLP signals")

    for forbidden in ["otlp-synthetic-secret", "customer-file.pdf", "alice@example.invalid", "disabled.action", "discarded.action"]:
        assert forbidden not in raw, f"privacy fixture leaked to collector: {forbidden}"

    logs, metrics, spans = [], [], []
    for batch in batches:
        for resource_key, scope_key, item_key, destination in [
            ("resourceLogs", "scopeLogs", "logRecords", logs),
            ("resourceMetrics", "scopeMetrics", "metrics", metrics),
            ("resourceSpans", "scopeSpans", "spans", spans),
        ]:
            for resource in batch.get(resource_key, []):
                identity = attributes(resource["resource"]["attributes"])
                assert identity["service.name"] == "otlp.conformance"
                assert identity["service.version"] == "1.0.0"
                assert identity["telemetry.sdk.name"] == "telemetry-cargo"
                for scope in resource[scope_key]:
                    destination.extend(scope[item_key])

    assert any(item["body"].get("stringValue") == "action" for item in logs)
    error = next(item for item in logs if item["body"].get("stringValue") == "error")
    assert error["severityNumber"] == 17
    assert attributes(error["attributes"])["telemetry.property.message"] == "[redacted]"
    counter = next(item for item in metrics if item["name"] == "telemetry.documents.imported")
    assert counter["sum"]["aggregationTemporality"] == 1
    assert not counter["sum"].get("isMonotonic", False)
    assert sum(point["asDouble"] for point in counter["sum"]["dataPoints"]) == 2
    for point in counter["sum"]["dataPoints"]:
        labels = attributes(point.get("attributes", []))
        assert "telemetry.event_id" not in labels
        assert "telemetry.session_id" not in labels
    span = next(item for item in spans if item["name"] == "document.ocr")
    assert len(span["traceId"]) == 32 and len(span["spanId"]) == 16
    assert int(span["endTimeUnixNano"]) - int(span["startTimeUnixNano"]) == 12_500_000
    print(f"Real collector accepted {len(logs)} logs, {sum(len(item['sum']['dataPoints']) for item in metrics)} metric points and {len(spans)} spans; privacy assertions passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", required=True)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    wait_for_receiver(options.endpoint)
    sdk = NativeSdk(options.library, {
        "app_id": "otlp.conformance",
        "app_version": "1.0.0",
        "endpoint": options.endpoint,
        "allow_insecure_loopback": True,
        "flush_interval_ms": 60_000,
    })
    try:
        sdk.call("action", id="disabled.action", result="success")
        assert sdk.call("diagnostics")["queue_depth"] == 0
        for category in ["diagnostics", "usage_aggregate", "usage_session", "performance"]:
            sdk.call("set_consent", category=category, enabled=True)
        sdk.call("action", id="search.submit", result="success")
        sdk.call("capture_error", code="import.failed", message="otlp-synthetic-secret /private/customer-file.pdf alice@example.invalid")
        sdk.call("increment", name="documents.imported", amount=3)
        sdk.call("increment", name="documents.imported", amount=-1)
        sdk.call("span", name="document.ocr", duration_ms=12.5)
        sdk.call("screen_enter", id="archive.search")
        sdk.call("screen_leave", id="archive.search")
        workflow = sdk.call("workflow_start", name="document.import")["id"]
        sdk.call("workflow_step", id=workflow, step="files_selected")
        sdk.call("workflow_end", id=workflow, outcome="complete")
        sdk.call("lifecycle", window_id="main", visible=True, focused=True)
        sdk.call("interaction")
        sdk.call("checkpoint")
        sdk.call("flush", timeout_ms=10_000)
        assert sdk.call("diagnostics")["queue_depth"] == 0
        sdk.call("action", id="discarded.action", result="success")
        sdk.call("set_consent", category="usage_aggregate", enabled=False)
        sdk.call("shutdown", timeout_ms=1000)
    finally:
        sdk.destroy()
    inspect_output(options.output)


if __name__ == "__main__":
    main()
