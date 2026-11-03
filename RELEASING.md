# Releasing

Release-binary hashes are updated once before tagging; they are independent of render hashes.

## Version policy

The Rust workspace / WASM engine and the TypeScript SDK are separate distribution packages and use independent Semantic Versioning. The unreleased working implementation uses Engine/WASM `0.4.0`, TypeScript SDK `0.5.0`, wire protocol `7`, document format `5`, package manifest format `2`, and commit format `2`. Package version numbers will be selected when preparing a release; these source changes do not publish one.

- Git tags and GitHub Release names follow the Engine/WASM version: `vX.Y.Z`.
- Record the Engine/WASM version, SDK version, and protocol version together in the GitHub Release notes. The GitHub Release publishes the WASM assets; it does not publish the SDK to npm.
- Update workspace crate versions and `verification/build.json`'s `version` together for an Engine/WASM release. Update `packages/alto-sdk/package.json` independently when the SDK package changes. Update both when a change affects both packages.
- A breaking document or wire protocol change must update its protocol version, migration guidance, README, and verification documents in the same change.

The working implementation accepts format 5 / render v3 only. Earlier files and protocols are intentionally unsupported: there has been no release requiring compatibility or migration. Rebuild the WASM/SDK together for this source; previously published WASM assets are not compatible.

1. Download `wasm-hashes-main` from the latest successful main CI run. The job builds the same source twice on arm64 macOS in separate clean targets and checks all hashes and sizes for reproducibility. Drift from committed values is report-only on main.
2. Open a PR replacing `wasm/SHA256SUMS` with the artifact's proposed `SHA256SUMS`. Merge the fields in `build-fields.json` (`wasmBytes`, `gzipBytes`, `wasmSha256`, and `comparisonWasm`) into `verification/build.json`, preserving its other fields. `sizes.json` contains sizes and hashes for every package file for review.
3. Merge that PR, wait for main CI to pass, then tag the merged main commit. If source changes land before tagging, repeat the update from the latest main artifact.

On tag pushes (`refs/tags/*`), `wasm-release-hashes` requires a clean build to match both committed files. It reports all package and metadata mismatches and fails before the slow tests in that job. To regenerate values locally, use `sh scripts/update-wasm-sums.sh` on aarch64-apple-darwin.
