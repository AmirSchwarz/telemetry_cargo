/// Native Dart/Flutter adapter. FFI runs in a dedicated isolate so export and
/// persistence never block the Flutter UI isolate. Browser targets unsupported.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';

enum ActionResult { success, failure, cancelled }

enum WorkflowOutcome { complete, cancelled, failed, timeout }

class TelemetryException implements Exception {
  final String message;
  const TelemetryException(this.message);
  @override
  String toString() => 'TelemetryException: $message';
}

/// A shared Rust engine owned by a worker isolate. All calls must be awaited
/// (or their returned errors explicitly handled). No global hooks are installed.
class Telemetry {
  final SendPort _worker;
  bool _closed = false;
  Future<void>? _closing;

  Telemetry._(this._worker);

  /// Config follows the canonical Rust JSON config. All optional categories
  /// default off. Pass the absolute path to a library built for this platform.
  static Future<Telemetry> initialize(
    Map<String, Object?> config, {
    String? libraryPath,
  }) async {
    final started = ReceivePort();
    try {
      await Isolate.spawn(
        _runWorker,
        _Start(config, libraryPath ?? _defaultLibrary(), started.sendPort),
        onError: started.sendPort,
      );
      final response = await started.first;
      if (response is SendPort) return Telemetry._(response);
      throw const TelemetryException('native initialization failed');
    } finally {
      started.close();
    }
  }

  Future<Map<String, Object?>> _request(
    Map<String, Object?> command, {
    bool close = false,
  }) async {
    final response = ReceivePort();
    try {
      _worker.send(_Request(command, close, response.sendPort));
      final value = await response.first;
      if (value is _Failure) throw TelemetryException(value.message);
      return Map<String, Object?>.from(value as Map);
    } finally {
      response.close();
    }
  }

  Future<Map<String, Object?>> command(Map<String, Object?> command) {
    if (_closed || _closing != null) {
      return Future.error(const TelemetryException('SDK is closed'));
    }
    return _request(command);
  }

  Future<void> setConsent(String category, bool enabled) async {
    await command({
      'op': 'set_consent',
      'category': category,
      'enabled': enabled,
    });
  }

  Future<void> action(
    String id, {
    ActionResult result = ActionResult.success,
  }) async {
    await command({'op': 'action', 'id': id, 'result': result.name});
  }

  Future<void> captureError(String code, String message) async {
    await command({'op': 'capture_error', 'code': code, 'message': message});
  }

  Future<void> increment(String name, num amount) async {
    await command({'op': 'increment', 'name': name, 'amount': amount});
  }

  Future<void> event(String name, Map<String, Object?> properties) async {
    await command({'op': 'event', 'name': name, 'properties': properties});
  }

  Future<void> registerEvent(Map<String, Object?> definition) async {
    await command({'op': 'register_event', 'definition': definition});
  }

  Future<void> screenEnter(String id) async {
    await command({'op': 'screen_enter', 'id': id});
  }

  Future<void> screenLeave(String id) async {
    await command({'op': 'screen_leave', 'id': id});
  }

  Future<void> lifecycle(
    String windowId, {
    required bool visible,
    required bool focused,
  }) async {
    await command({
      'op': 'lifecycle',
      'window_id': windowId,
      'visible': visible,
      'focused': focused,
    });
  }

  Future<void> interaction() async {
    await command({'op': 'interaction'});
  }

  Future<void> suspend(bool suspended) async {
    await command({'op': 'suspend', 'suspended': suspended});
  }

  Future<void> checkpoint() async {
    await command({'op': 'checkpoint'});
  }

  Future<String> workflowStart(String name) async {
    final response = await command({'op': 'workflow_start', 'name': name});
    return response['id'] as String;
  }

  Future<void> workflowStep(String id, String step) async {
    await command({'op': 'workflow_step', 'id': id, 'step': step});
  }

  Future<void> workflowEnd(String id, WorkflowOutcome outcome) async {
    await command({'op': 'workflow_end', 'id': id, 'outcome': outcome.name});
  }

  /// Disabled performance consent retains no timing state. Consent changes
  /// during the operation invalidate the measurement. App exceptions survive
  /// even if recording the span also fails.
  Future<T> span<T>(String name, FutureOr<T> Function() operation) async {
    final state = await diagnostics();
    final consent = state['consent'] as Map;
    if (consent['performance'] != true) return await operation();
    final epoch = state['consent_epoch'] as int;
    final timer = Stopwatch()..start();
    Future<void> finish() async {
      timer.stop();
      await command({
        'op': 'span',
        'name': name,
        'duration_ms': timer.elapsedMicroseconds / 1000,
        'consent_epoch': epoch,
      });
    }

    final T result;
    try {
      result = await operation();
    } catch (error, stack) {
      try {
        await finish();
      } catch (_) {
        /* Preserve the original app error. */
      }
      Error.throwWithStackTrace(error, stack);
    }
    await finish();
    return result;
  }

  Future<Map<String, Object?>> diagnostics() => command({'op': 'diagnostics'});
  Future<Map<String, Object?>> capabilities() =>
      command({'op': 'capabilities'});

  Future<void> flush({Duration timeout = const Duration(seconds: 5)}) async {
    await command({'op': 'flush', 'timeout_ms': timeout.inMilliseconds});
  }

  /// Always releases the native handle, including when final export fails.
  Future<void> close({Duration timeout = const Duration(seconds: 5)}) {
    if (_closing != null) return _closing!;
    if (_closed) return Future.value();
    return _closing = _close(timeout);
  }

  Future<void> _close(Duration timeout) async {
    try {
      await _request({
        'op': 'shutdown',
        'timeout_ms': timeout.inMilliseconds,
      }, close: true);
    } finally {
      _closed = true;
    }
  }
}

String _defaultLibrary() {
  if (Platform.isWindows) return 'telemetry_ffi.dll';
  if (Platform.isMacOS || Platform.isIOS) return 'libtelemetry_ffi.dylib';
  return 'libtelemetry_ffi.so';
}

class _Start {
  final Map<String, Object?> config;
  final String library;
  final SendPort response;
  const _Start(this.config, this.library, this.response);
}

class _Request {
  final Map<String, Object?> command;
  final bool close;
  final SendPort response;
  const _Request(this.command, this.close, this.response);
}

class _Failure {
  final String message;
  const _Failure(this.message);
}

void _runWorker(_Start start) {
  final _Native native;
  try {
    native = _Native(start.library, start.config);
  } catch (_) {
    start.response.send(const _Failure('native initialization failed'));
    return;
  }
  final port = ReceivePort();
  start.response.send(port.sendPort);
  port.listen((message) {
    final request = message as _Request;
    Object result;
    try {
      result = native.command(request.command);
    } catch (_) {
      result = const _Failure('native operation failed');
    } finally {
      if (request.close) {
        native.destroy();
        port.close();
      }
    }
    request.response.send(result);
  });
}

final class _Buffer extends Struct {
  external Pointer<Uint8> data;
  @UintPtr()
  external int len;
}

typedef _InitNative = Int32 Function(Pointer<Uint8>, UintPtr, Pointer<Uint64>);
typedef _InitDart = int Function(Pointer<Uint8>, int, Pointer<Uint64>);
typedef _CommandNative =
    Int32 Function(Uint64, Pointer<Uint8>, UintPtr, Pointer<_Buffer>);
typedef _CommandDart = int Function(int, Pointer<Uint8>, int, Pointer<_Buffer>);
typedef _DestroyNative = Int32 Function(Uint64);
typedef _DestroyDart = int Function(int);
typedef _BufferFreeNative = Void Function(_Buffer);
typedef _BufferFreeDart = void Function(_Buffer);
typedef _MallocNative = Pointer<Void> Function(UintPtr);
typedef _MallocDart = Pointer<Void> Function(int);
typedef _FreeNative = Void Function(Pointer<Void>);
typedef _FreeDart = void Function(Pointer<Void>);

class _Native {
  late final _InitDart _init;
  late final _CommandDart _command;
  late final _DestroyDart _destroy;
  late final _BufferFreeDart _bufferFree;
  late final _MallocDart _malloc;
  late final _FreeDart _free;
  int _handle = 0;

  _Native(String path, Map<String, Object?> config) {
    final library = DynamicLibrary.open(path);
    _init = library.lookupFunction<_InitNative, _InitDart>('telemetry_v1_init');
    _command = library.lookupFunction<_CommandNative, _CommandDart>(
      'telemetry_v1_command',
    );
    _destroy = library.lookupFunction<_DestroyNative, _DestroyDart>(
      'telemetry_v1_destroy',
    );
    _bufferFree = library.lookupFunction<_BufferFreeNative, _BufferFreeDart>(
      'telemetry_v1_buffer_free',
    );
    final libc = Platform.isWindows
        ? DynamicLibrary.open('msvcrt.dll')
        : DynamicLibrary.process();
    _malloc = libc.lookupFunction<_MallocNative, _MallocDart>('malloc');
    _free = libc.lookupFunction<_FreeNative, _FreeDart>('free');
    final bytes = utf8.encode(jsonEncode(config));
    final input = _allocate<Uint8>(bytes.length);
    final output = _allocate<Uint64>(sizeOf<Uint64>());
    try {
      input.asTypedList(bytes.length).setAll(0, bytes);
      if (_init(input, bytes.length, output) != 0)
        throw const TelemetryException('invalid config');
      _handle = output.value;
    } finally {
      _free(input.cast());
      _free(output.cast());
    }
  }

  Pointer<T> _allocate<T extends NativeType>(int size) {
    if (size <= 0 || size > 1024 * 1024)
      throw const TelemetryException('input budget exceeded');
    final pointer = _malloc(size);
    if (pointer == nullptr)
      throw const TelemetryException('native allocation failed');
    return pointer.cast<T>();
  }

  Map<String, Object?> command(Map<String, Object?> command) {
    final bytes = utf8.encode(jsonEncode(command));
    final input = _allocate<Uint8>(bytes.length);
    final output = _allocate<_Buffer>(sizeOf<_Buffer>());
    output.ref.data = nullptr;
    output.ref.len = 0;
    try {
      input.asTypedList(bytes.length).setAll(0, bytes);
      final status = _command(_handle, input, bytes.length, output);
      if (status != 0) throw const TelemetryException('native command failed');
      if (output.ref.data == nullptr || output.ref.len > 1024 * 1024) {
        throw const TelemetryException('invalid native response');
      }
      final response = utf8.decode(output.ref.data.asTypedList(output.ref.len));
      return Map<String, Object?>.from(jsonDecode(response) as Map);
    } finally {
      _bufferFree(output.ref);
      _free(input.cast());
      _free(output.cast());
    }
  }

  void destroy() {
    if (_handle != 0) {
      _destroy(_handle);
      _handle = 0;
    }
  }
}
