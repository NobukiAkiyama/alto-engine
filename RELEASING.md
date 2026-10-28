# Releasing

Release-binary hashes are updated once before tagging; they are independent of render hashes.

1. Download `wasm-hashes-main` from the latest successful main CI run. The job builds the same source twice on arm64 macOS in separate clean targets and checks all hashes and sizes for reproducibility. Drift from committed values is report-only on main.
2. Open a PR replacing `wasm/SHA256SUMS` with the artifact's proposed `SHA256SUMS`. Merge the fields in `build-fields.json` (`wasmBytes`, `gzipBytes`, `wasmSha256`, and `comparisonWasm`) into `verification/build.json`, preserving its other fields. `sizes.json` contains sizes and hashes for every package file for review.
3. Merge that PR, wait for main CI to pass, then tag the merged main commit. If source changes land before tagging, repeat the update from the latest main artifact.

On tag pushes (`refs/tags/*`), `wasm-release-hashes` requires a clean build to match both committed files. It reports all package and metadata mismatches and fails before the slow tests in that job. To regenerate values locally, use `sh scripts/update-wasm-sums.sh` on aarch64-apple-darwin.
