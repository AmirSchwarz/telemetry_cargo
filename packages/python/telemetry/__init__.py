"""Privacy-gated telemetry. Optional categories default to disabled.

Build with maturin or run packages/python/build_native.py for local development.
All collection, schemas, consent and queue behavior are owned by Rust.
"""
from __future__ import annotations

from contextlib import contextmanager
import json
import logging
import time
from typing import Any, Iterator, Mapping, Optional

from ._native import NativeTelemetry


class Telemetry:
    def __init__(self, config: Mapping[str, Any]):
        self._native = NativeTelemetry(json.dumps(dict(config), allow_nan=False))

    def _command(self, op: str, **fields: Any) -> Any:
        return json.loads(self._native.command(json.dumps({"op": op, **fields}, allow_nan=False)))

    def set_consent(self, category: str, enabled: bool) -> None:
        self._command("set_consent", category=category, enabled=enabled)

    def event(self, name: str, properties: Optional[Mapping[str, Any]] = None) -> None:
        self._command("event", name=name, properties=dict(properties or {}))

    def register_event(self, definition: Mapping[str, Any]) -> None:
        self._command("register_event", definition=dict(definition))

    def action(self, identifier: str, result: str = "success") -> None:
        self._command("action", id=identifier, result=result)

    def capture_error(self, code: str, message: str = "") -> None:
        self._command("capture_error", code=code, message=message)

    def increment(self, name: str, amount: float = 1) -> None:
        self._command("increment", name=name, amount=amount)

    def screen_enter(self, identifier: str) -> None:
        self._command("screen_enter", id=identifier)

    def screen_leave(self, identifier: str) -> None:
        self._command("screen_leave", id=identifier)

    def workflow_start(self, name: str) -> str:
        return self._command("workflow_start", name=name)["id"]

    def workflow_step(self, identifier: str, step: str) -> None:
        self._command("workflow_step", id=identifier, step=step)

    def workflow_end(self, identifier: str, outcome: str = "complete") -> None:
        self._command("workflow_end", id=identifier, outcome=outcome)

    @contextmanager
    def span(self, name: str) -> Iterator[None]:
        # Do not even retain a span start while this category is disabled.
        diagnostics = self.diagnostics()
        allowed = diagnostics.get("consent", {}).get("performance", False)
        if not allowed:
            yield
            return
        start = time.monotonic_ns()
        epoch = diagnostics["consent_epoch"]
        try:
            yield
        except BaseException:
            # A telemetry validation/export failure must preserve the app error.
            try:
                self._finish_span(name, start, epoch)
            except Exception:
                pass
            raise
        else:
            self._finish_span(name, start, epoch)

    def _finish_span(self, name: str, start: int, epoch: int) -> None:
        current = self.diagnostics()
        if current["consent_epoch"] == epoch and current["consent"].get("performance", False):
            self._command("span", name=name, duration_ms=(time.monotonic_ns() - start) / 1_000_000, consent_epoch=epoch)

    def lifecycle(self, window_id: str, visible: bool, focused: bool) -> None:
        self._command("lifecycle", window_id=window_id, visible=visible, focused=focused)

    def interaction(self) -> None:
        self._command("interaction")

    def suspend(self, suspended: bool) -> None:
        self._command("suspend", suspended=suspended)

    def checkpoint(self) -> None:
        self._command("checkpoint")

    def diagnostics(self) -> dict[str, Any]:
        return self._command("diagnostics")

    def capabilities(self) -> dict[str, Any]:
        return {**self._command("capabilities"), "binding": "pyo3"}

    def flush(self, timeout_ms: int = 5000) -> None:
        self._command("flush", timeout_ms=timeout_ms)

    def shutdown(self, timeout_ms: int = 5000) -> None:
        self._native.close(timeout_ms)

    close = shutdown

    def __enter__(self) -> "Telemetry":
        return self

    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> None:
        if exc_type is None:
            self.shutdown()
        else:
            try:
                self.shutdown()
            except Exception:
                pass


def init(config: Mapping[str, Any]) -> Telemetry:
    return Telemetry(config)


class TelemetryHandler(logging.Handler):
    """Opt-in logging adapter; never sends formatted logging text or exception contents.

    Every ERROR-or-higher entry emits the semantic error code ``python.logging``.
    No logger names, filenames, tracebacks or free-form messages are retained.
    Standard logging handlers remain in place.
    """

    def __init__(self, sdk: Telemetry):
        super().__init__(logging.ERROR)
        self.sdk = sdk

    def emit(self, record: logging.LogRecord) -> None:
        try:
            self.sdk.capture_error("python.logging")
        except (RuntimeError, ValueError):
            # Telemetry must not make application logging fail or recurse.
            pass


__all__ = ["Telemetry", "TelemetryHandler", "init"]
