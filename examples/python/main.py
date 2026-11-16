"""Run with PYTHONPATH=packages/python python3 examples/python/main.py."""
import telemetry

sdk = telemetry.init({"app_id": "example.python", "app_version": "1.0.0"})
sdk.action("search.submit")  # Disabled by default: nothing retained.
assert sdk.diagnostics()["queue_depth"] == 0
sdk.set_consent("usage_aggregate", True)
sdk.action("search.submit")
sdk.increment("documents.imported", 3)
sdk.set_consent("diagnostics", True)
sdk.capture_error("document.failed", "synthetic secret is redacted")
print(sdk.capabilities())
print(sdk.diagnostics())
# No endpoint in this example. Withdrawal purges the local demonstration data.
sdk.set_consent("usage_aggregate", False)
sdk.set_consent("diagnostics", False)
sdk.shutdown()
