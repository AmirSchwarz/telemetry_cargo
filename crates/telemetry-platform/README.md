# Telemetry platform

`capabilities()` explicitly reports supported low-level measurements. The default
build has all sampling disabled. The `process-metrics` feature enables Linux
current-process RSS bytes and cumulative user-plus-system CPU clock ticks.
No background sampling is started. Call `ProcessSampler::sample()` only after
the application's performance consent gate has allowed collection, and at a
coarse interval such as 30–60 seconds.

`ProcessSample` contains monotonic `elapsed_ms`, `rss_bytes`, and `cpu_ticks`.
CPU ticks cover the process, including its threads; they are not CPU utilization
or milliseconds. Converting ticks to seconds requires this host's `CLK_TCK`.
The sampler deliberately makes no fixed tick-rate assumption. Linux procfs
must be mounted and readable; unavailable or invalid measurements return errors.

Other operating systems, CPU percentage, GPU, battery/energy, crash dumps, and
automatic UI/window state detection are explicitly unsupported. UI adapters
must supply normalized lifecycle state. Sampling never returns paths, process
IDs, executable names, device IDs, or other applications' measurements.
