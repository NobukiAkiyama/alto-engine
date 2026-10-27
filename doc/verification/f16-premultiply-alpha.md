# Semi-transparent v2 parity, measured not enforced

Render version 2 composite parity (`composite.wgsl` against the CPU reference) previously locked 37 cases. Several of those cases are opaque, or use only a few straight alphas, so a full RGB sweep at alpha 0, 1, 128, and 254 was not part of the enforced set. Each enforced case now has an `-alpha-measure` twin with that sweep. The twins are labeled **measured-not-enforced**: the job prints the mismatch count, the max absolute difference per channel, and the first mismatching pixels (source, GPU, CPU), and a mismatch does not fail CI. They are not written to `tests/parity/cpu-reference.txt`. No shader, formula, or recorded hash changed.

Source texels are straight RGBA. R is `x & 255`, G is `y & 255`, B is `(3x+5y) & 255`, and alpha cycles through 0, 1, 128, 254. `source` in the report is the straight texel at the output pixel's document position. An identity layer uses that texel. A transformed layer samples elsewhere.

The color-adjust effect pass is a different shader (`rgba16float`) and stays an explicit xfail. This file measures the composite cases only.

`extremes-alpha-measure` pixel (80,2) is source `80,2,250,128` (alpha 128). On lavapipe that pixel matches the CPU reference.

## lavapipe

Backend: `llvmpipe (LLVM 20.1.2, 256 bits)`. Runner measurement from this workspace's Mesa lavapipe. CI records the same job on `ubuntu-24.04` in artifact `gpu-parity-report` and the job summary.

| case | mismatched pixels | max abs r,g,b,a |
| --- | ---: | --- |
| blend-normal-alpha-measure | 0 | 0,0,0,0 |
| blend-multiply-alpha-measure | 0 | 0,0,0,0 |
| blend-screen-alpha-measure | 0 | 0,0,0,0 |
| blend-overlay-alpha-measure | 0 | 0,0,0,0 |
| blend-darken-alpha-measure | 0 | 0,0,0,0 |
| blend-lighten-alpha-measure | 0 | 0,0,0,0 |
| blend-color_dodge-alpha-measure | 0 | 0,0,0,0 |
| blend-color_burn-alpha-measure | 0 | 0,0,0,0 |
| blend-hard_light-alpha-measure | 0 | 0,0,0,0 |
| blend-soft_light-alpha-measure | 0 | 0,0,0,0 |
| blend-difference-alpha-measure | 0 | 0,0,0,0 |
| blend-exclusion-alpha-measure | 0 | 0,0,0,0 |
| blend-hue-alpha-measure | 0 | 0,0,0,0 |
| blend-saturation-alpha-measure | 0 | 0,0,0,0 |
| blend-color-alpha-measure | 0 | 0,0,0,0 |
| blend-luminosity-alpha-measure | 0 | 0,0,0,0 |
| blend-add-alpha-measure | 0 | 0,0,0,0 |
| opacity-half-alpha-measure | 0 | 0,0,0,0 |
| opacity-zero-alpha-measure | 0 | 0,0,0,0 |
| hidden-layer-alpha-measure | 0 | 0,0,0,0 |
| isolated-group-alpha-measure | 0 | 0,0,0,0 |
| pass-through-group-alpha-measure | 0 | 0,0,0,0 |
| nested-groups-alpha-measure | 0 | 0,0,0,0 |
| translate-across-tile-alpha-measure | 0 | 0,0,0,0 |
| translate-across-tile-1-alpha-measure | 0 | 0,0,0,0 |
| half-pixel-translate-alpha-measure | 0 | 0,0,0,0 |
| scale-bilinear-alpha-measure | 0 | 0,0,0,0 |
| extremes-alpha-measure | 0 | 0,0,0,0 |
| alpha-extremes-alpha-measure | 0 | 0,0,0,0 |
| two-partial-alpha-measure | 0 | 0,0,0,0 |
| eight-layers-alpha-measure | 0 | 0,0,0,0 |
| odd-canvas-0-alpha-measure | 0 | 0,0,0,0 |
| odd-canvas-1-alpha-measure | 0 | 0,0,0,0 |
| level1-centers-alpha-measure | 0 | 0,0,0,0 |
| rotation-alpha-measure | 0 | 0,0,0,0 |
| large-translation-alpha-measure | 0 | 0,0,0,0 |
| large-translation-out-of-range-alpha-measure | 0 | 0,0,0,0 |

All 37 lavapipe cases matched. There were no mismatching pixels to list.

## Metal

Filled from this PR's `alpha-measure-metal` artifact (`macos-15`, native `cargo test`).

## SwiftShader

Filled from this PR's `alpha-measure-swiftshader` artifact (wasm job, Chrome, `ALTO_BROWSER_GPU=swiftshader`). The CPU side of that run is the JavaScript reference in `tests/e2e/cpu-composite.js`, not the Rust reference used above.
