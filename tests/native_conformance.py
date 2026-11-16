"""Run identical protocol commands through the compiled C ABI.

Uses only Python's standard library. Build telemetry-ffi first, then run:
    python3 tests/native_conformance.py target/debug/libtelemetry_ffi.so
"""

from __future__ import annotations

import ctypes
import json
from pathlib import Path
import sys
import unittest


class Buffer(ctypes.Structure):
    _fields_ = [("data", ctypes.POINTER(ctypes.c_ubyte)), ("len", ctypes.c_size_t)]


class NativeConformance(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lib = ctypes.CDLL(str(Path(LIBRARY).resolve()))
        cls.lib.telemetry_v1_init.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_uint64)]
        cls.lib.telemetry_v1_init.restype = ctypes.c_int
        cls.lib.telemetry_v1_command.argtypes = [ctypes.c_uint64, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(Buffer)]
        cls.lib.telemetry_v1_command.restype = ctypes.c_int
        cls.lib.telemetry_v1_destroy.argtypes = [ctypes.c_uint64]
        cls.lib.telemetry_v1_destroy.restype = ctypes.c_int
        cls.lib.telemetry_v1_buffer_free.argtypes = [Buffer]
        cls.lib.telemetry_v1_buffer_free.restype = None
        cls.fixture = json.loads((Path(__file__).parent / "fixtures/commands-v1.json").read_text())

    def setUp(self):
        config = json.dumps(self.fixture["config"]).encode()
        self.handle = ctypes.c_uint64()
        status = self.lib.telemetry_v1_init(config, len(config), ctypes.byref(self.handle))
        self.assertEqual(status, 0)

    def tearDown(self):
        if self.handle.value:
            self.assertEqual(self.lib.telemetry_v1_destroy(self.handle.value), 0)

    def command(self, command):
        request = json.dumps(command).encode()
        output = Buffer()
        status = self.lib.telemetry_v1_command(self.handle.value, request, len(request), ctypes.byref(output))
        try:
            value = json.loads(ctypes.string_at(output.data, output.len)) if output.data else None
        finally:
            self.lib.telemetry_v1_buffer_free(output)
        return status, value

    def test_shared_commands(self):
        depths = []
        for command in self.fixture["commands"]:
            status, value = self.command(command)
            self.assertEqual(status, 0, value)
            if command["op"] == "diagnostics":
                depths.append(value["queue_depth"])
        self.assertEqual(depths, self.fixture["expected_queue_depths"])

    def test_unknown_fields_and_utf8_are_rejected(self):
        status, _ = self.command({"op": "increment", "name": "files.imported", "amount": 2, "secret": "discard_me"})
        self.assertNotEqual(status, 0)
        output = Buffer()
        status = self.lib.telemetry_v1_command(self.handle.value, b"\xff", 1, ctypes.byref(output))
        self.assertNotEqual(status, 0)
        self.lib.telemetry_v1_buffer_free(output)

    def test_destroyed_handles_cannot_be_reused(self):
        stale = self.handle.value
        self.assertEqual(self.lib.telemetry_v1_destroy(stale), 0)
        self.handle.value = 0
        self.assertNotEqual(self.lib.telemetry_v1_destroy(stale), 0)
        output = Buffer()
        request = b'{"op":"diagnostics"}'
        self.assertNotEqual(self.lib.telemetry_v1_command(stale, request, len(request), ctypes.byref(output)), 0)
        self.lib.telemetry_v1_buffer_free(output)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit("usage: python3 tests/native_conformance.py /absolute/path/to/native/library")
    LIBRARY = sys.argv.pop(1)
    unittest.main()
