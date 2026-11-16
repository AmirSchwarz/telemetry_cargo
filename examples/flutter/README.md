The executable native Dart smoke example is
`packages/flutter/example/smoke.dart`. It exercises the same package usable by
native Flutter applications, without requiring a generated Flutter app.

From the repository root:

```sh
cargo build --release -p telemetry-ffi
dart run packages/flutter/example/smoke.dart "$PWD/target/release/libtelemetry_ffi.so"
```

See `packages/flutter/README.md` for packaging and unverified mobile limitations.
