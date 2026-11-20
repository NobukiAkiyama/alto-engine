# Releasing

Release-binary hashes are updated once before tagging; they are independent of render hashes.

## Version policy

The Rust workspace / WASM engine and the TypeScript SDK are separate distribution packages and use independent Semantic Versioning. The unreleased working implementation uses Engine/WASM `0.4.0`, TypeScript SDK `0.5.0`, wire protocol `17`, document format `8`, package manifest formats `2`–`4`, and commit format `2`. Package version numbers will be selected when preparing a release; these source changes do not publish one.

- Git tags and GitHub Release names follow the Engine/WASM version: `vX.Y.Z`.
- Record the Engine/WASM version, SDK version, and protocol version together in the GitHub Release notes. The GitHub Release publishes the WASM assets; it does not publish the SDK to npm.
- Update workspace crate versions and `verification/build.json`'s `version` together for an Engine/WASM release. Update `packages/alto-sdk/package.json` independently when the SDK package changes. Update both when a change affects both packages.
- A breaking document or wire protocol change must update its protocol version, migration guidance, README, and verification documents in the same change.

The working implementation accepts formats 5, 6, 7, and 8 / render v3 only. Earlier files and protocols are intentionally unsupported: there has been no release requiring compatibility or migration. Rebuild the WASM/SDK together for this source; previously published WASM assets are not compatible.

Protocol 11 replaces `BrushOptions.pressureSize` and `pressureOpacity` with `dynamics`. Omit `dynamics.size` for the default pressure-sensitive size, or set it to `null` for a fixed size. Replace `pressureOpacity: true` with `dynamics.opacity: { input: "pressure", points: [[0, 0], [1, 1]] }`. Omit that curve for constant dab opacity. Rebuild the SDK, Worker, and WASM as a set. Rust callers of `StrokeSampler::new` handle its validation `Result`.

Protocol 12 carries image brush coverage in `beginBrush`. Rebuild the SDK, Worker, and WASM together to enable `assets.importBrushTip` and `BrushOptions.tip.image`. Painted coverage is stored as ordinary document tiles.

Protocol 13 carries the repeating brush texture and its scale and depth in `beginBrush`. Rebuild the SDK, Worker, and WASM together to enable `assets.importBrushTexture` and `BrushOptions.tip.texture`.

Protocol 14 adds stash operations. Rebuild the SDK, Worker, and WASM together to use them. Packages with a stash use manifest format 4, for both full and thin archives. Packages with an empty stash list continue to use format 2 (full) or 3 (thin).

Protocol 15 adds JPEG and TIFF export options. Rebuild the SDK, Worker, and WASM together. JPEG composites onto an sRGB background. TIFF writes straight-alpha RGBA8 with optional Deflate compression.

1. Download `wasm-hashes-main` from the latest successful main CI run. The job builds the same source twice on arm64 macOS in separate clean targets and checks all hashes and sizes for reproducibility. Drift from committed values is report-only on main.
2. Open a PR replacing `wasm/SHA256SUMS` with the artifact's proposed `SHA256SUMS`. Merge the fields in `build-fields.json` (`wasmBytes`, `gzipBytes`, `wasmSha256`, and `comparisonWasm`) into `verification/build.json`, preserving its other fields. `sizes.json` contains sizes and hashes for every package file for review.
3. Merge that PR, wait for main CI to pass, then tag the merged main commit. If source changes land before tagging, repeat the update from the latest main artifact.

On tag pushes (`refs/tags/*`), `wasm-release-hashes` requires a clean build to match both committed files. It reports all package and metadata mismatches and fails before the slow tests in that job. To regenerate values locally, use `sh scripts/update-wasm-sums.sh` on aarch64-apple-darwin.

## SDK changes for the next release

`SelectionSpec` is a discriminated union, and `SelectionMode` is exported. Supply `points` for a lasso, omit `mode` for `clear`, and narrow the selection kind when constructing options dynamically. The new `color` and `contiguous` variants use integer inputs and keep an empty selection when nothing matches. Fill rejects that state with `EMPTY_SELECTION`, and inverting it selects the whole document. This is a breaking TypeScript change to record in the SDK release notes.
