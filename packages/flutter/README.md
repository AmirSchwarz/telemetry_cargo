# Native Dart / Flutter telemetry

`telemetry_native` is a Dart FFI package usable from native Flutter apps. It has
no third-party Dart dependencies. Native work runs in a dedicated isolate;
await each API call and handle errors. It does not support Flutter Web.

```sh
cargo build --release -p telemetry-ffi
dart analyze packages/flutter/lib packages/flutter/example
dart run packages/flutter/example/smoke.dart "$PWD/target/release/libtelemetry_ffi.so"
```

```dart
final sdk = await Telemetry.initialize(
  {'app_id': 'my.app', 'app_version': '1.0'},
  libraryPath: '/absolute/path/libtelemetry_ffi.so',
);
await sdk.setConsent('usage_aggregate', true);
await sdk.action('search.submit', result: ActionResult.success);
await sdk.increment('documents.imported', 3);
await sdk.close();
```

The host owns consent UX and all lifecycle integration. Call
`lifecycle('main', visible: true, focused: true)`, `interaction()` without input
contents, and `suspend(true/false)` from the appropriate framework callbacks.
Use stable route/widget identifiers with `screenEnter` and `screenLeave`.
Automatic route/widget hooks and platform lifecycle plugins are not bundled.

The app must package a matching `libtelemetry_ffi.so` (Linux/Android),
`libtelemetry_ffi.dylib` (macOS), or `telemetry_ffi.dll` (Windows), and pass its
loadable path. iOS static linking/entitlements, Android ABI packaging, store
distribution, signing, and platform tests are not provided or verified here.
Do not infer mobile support from the portable Dart source alone. This package
loads a dynamic library, so iOS deployment requires additional integration.

Optional telemetry starts off. Raw error messages are replaced before queueing.
Config and command JSON match the canonical Rust protocol. `capabilities()`
reports the engine's available features; it does not certify platform packaging.
`close(timeout: ...)` attempts shutdown then always releases native resources;
failed final export is reported. Browser/Web must use the JS/WASM package.
