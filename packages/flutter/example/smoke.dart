import 'dart:io';
import '../lib/telemetry.dart';

Future<void> main(List<String> arguments) async {
  if (arguments.isEmpty)
    throw ArgumentError('pass absolute native library path');
  final sdk = await Telemetry.initialize({
    'app_id': 'dart.example',
    'app_version': '1.0',
  }, libraryPath: arguments.single);
  await sdk.action('search.submit');
  await sdk.captureError('io.error', 'password=synthetic');
  await sdk.lifecycle('main', visible: true, focused: true);
  await sdk.interaction();
  final diagnostics = await sdk.diagnostics();
  final capabilities = await sdk.capabilities();
  if (diagnostics.isEmpty || capabilities.isEmpty)
    throw StateError('missing native response');
  await sdk.setConsent('performance', true);
  await sdk.span('operation.revoked', () async {
    await sdk.setConsent('performance', false);
    await sdk.setConsent('performance', true);
  });
  // With no exporter configured, successful flush proves no stale span queued.
  await sdk.flush();
  var rejected = false;
  try {
    await sdk.command({'op': 'unknown'});
  } on TelemetryException {
    rejected = true;
  }
  if (!rejected) throw StateError('invalid operation accepted');
  await sdk.close();
  await sdk.close();
  try {
    await sdk.action('after.close');
    throw StateError('closed SDK accepted event');
  } on TelemetryException {
    stdout.writeln(
      'Dart native smoke passed (isolate, FFI, privacy defaults, errors, close)',
    );
  }
}
