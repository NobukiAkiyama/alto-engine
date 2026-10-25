# GPU surface isolation probes

These are investigation fixtures, not product examples or performance benchmarks.

Run a raw JavaScript WebGPU probe against the installed Chrome:

```sh
ALTO_BROWSER_GPU=swiftshader \
ALTO_BROWSER_REPORT_DIR=/tmp/alto-gpu-probe \
ALTO_GPU_LIFECYCLE_MODE=replace-abrupt \
node scripts/verify-browser.mjs --gpu-lifecycle
```

Set `CHROME` and `ALTO_BROWSER_EXPECTED_VERSION` to use an exact browser build.

- `ALTO_GPU_LIFECYCLE_MODE`: `stable`, `same-size`, `resize`, `same-size-burst`, `resize-burst`, `replace-abrupt`, `replace-graceful`, `recover`, `recover-unconfigure`, `backlog-resize`, `backlog-replace`.
- `ALTO_GPU_LIFECYCLE_CYCLES`: 1–2000, default 120. Keep WASM runs small (e.g. 20); full Runtime initialization and compilation are substantially slower than the raw probe. The overall evaluation deadline is 180 seconds; exceeding that deadline is a harness limit, not proof of a GPU failure.
- `ALTO_GPU_LIFECYCLE_MEMORY_MIB`: 0 or 192. The raw probe can create three 64 MiB texture arrays. This is a resource allocation control; it does not measure resident GPU memory or reproduce Alto's workloads. The WASM probe uses Runtime's own allocations.
- `ALTO_GPU_LIFECYCLE_IMPLEMENTATION`: `webgpu` or `wasm`. The latter uses the shipped WASM Runtime, an empty document, frame validation and content tile readback. It excludes SDK scheduling, recovery storage, assets and painting.
- `ALTO_BROWSER_GRAPHITE_BACKEND=swiftshader`: explicitly compare the display compositor's Graphite/Dawn backend with SwiftShader. The harness asserts `GraphiteDawnVulkan`; the default preserves browser behavior. Linux launch flags differ from macOS, and a requested adapter alone does not establish a working display path.
- `ALTO_BROWSER_GPU_TRACE=1`: capture a bounded, 64 MiB circular Chrome GPU trace. The exported JSON can be larger than the binary trace buffer. Artifacts include the browser's SharedImage create/destroy events and Dawn command execution spans.

Raw WebGPU frames clear the actual canvas surface green, copy one surface pixel to a mapped buffer and assert `[0,255,0,255]`. Each condition runs in a fresh browser profile. Graceful termination waits for submitted work, unconfigures the context and destroys the device; abrupt termination submits and terminates without that wait. Expected device destruction is distinguished from unexpected loss.

`--functional` still runs the actual SDK correctness suites. The manual CI input `lifecycle_implementation=sdk` selects those suites in the platform comparison job, rather than an empty Runtime probe.

The Linux comparison remains experimental. The observed `Skia=None` / missing SharedImage factory failures and the subsequent Linux GPU process crashes are distinct from the original macOS failure, whose GPU process remains alive. Do not combine these outcomes into one root cause or count setup errors as successful reproductions.

The backlog probes submit a bounded compute workload before each present without
waiting for GPU completion on each iteration. The final frame still checks a
mapped surface pixel. `backlog-resize` changes the surface dimensions while work
is queued; `backlog-replace` terminates and replaces the Worker every ten submits.
This tests work in flight, which allocation-only controls do not establish.

Manual CI defaults to `macos-15`. Select `diagnostic_runner=ubuntu-24.04` only for
the experimental platform comparison. Normal `check` CI retains its release build
host and all browser correctness suites.

`--startup-only` opens the actual Playground/SDK, waits for a settled frame and
snapshot, and rejects any missing SharedImage mailbox observed during that fresh
startup even if automatic GPU recovery subsequently succeeds. Manual CI input
`lifecycle_implementation=startup` performs five independent fresh-profile
startups, retaining every result; it does not retry a failed test to obtain a pass.
The longer 45-second trace drain is diagnostic collection only; the engine's
initialization and operation timeouts are unchanged.
