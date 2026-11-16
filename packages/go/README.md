# Go telemetry

This source package uses the same Rust engine as the Rust, Python and Node APIs.
It requires **cgo** and a native library built for the target OS and architecture.
`CGO_ENABLED=0` compiles but `New` returns `ErrCGODisabled`; a pure-Go fallback is
not implemented. No native binaries are bundled or signed yet.

From the repository root:

```sh
cargo build --release -p telemetry-ffi
cd packages/go
CGO_ENABLED=1 LD_LIBRARY_PATH=../../target/release go test ./...
```

On macOS use `DYLD_LIBRARY_PATH`; on Windows put the matching DLL on `PATH`.
For copying the package outside this workspace, install `include/telemetry.h`
and the target library and update the cgo include/linker paths in
`native_cgo.go`. The placeholder module namespace is `telemetry.local/sdk`.

```go
sdk, err := telemetry.New(telemetry.Config{AppID: "my.app", AppVersion: "1.0"})
if err != nil { return err }
defer sdk.Close(ctx)
// Consent belongs to the host application's settings/UI.
if err := sdk.SetConsent(telemetry.UsageAggregate, true); err != nil { return err }
if err := sdk.Action("search.submit", telemetry.Success); err != nil { return err }
```

All optional categories start disabled. Raw error messages are discarded by the
engine's privacy gate. Use semantic identifiers, never user names, paths or query
text. `Command` exposes the shared JSON protocol for registered custom events.
Check returned errors. `Flush` and `Close` block the caller and obey a supplied
context deadline; context cancellation without a deadline cannot interrupt an
already active FFI call. Use a worker goroutine for UI applications. Native
lifecycle detection is the host's responsibility via `Lifecycle`, `Interaction`
and `Suspend`; capability discovery reports engine capabilities.

`Close` immediately marks the wrapper closed and starts shutdown without waiting
for concurrent native calls. Already-started calls may finish or return a native
closed/invalid-handle error; calls started after `Close` return `ErrClosed`.
