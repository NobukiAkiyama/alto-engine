# Semi-transparent v2 parity, measured not enforced

Render version 2 composite parity (`composite.wgsl` against the CPU reference) previously locked 37 cases. Several of those cases are opaque, or use only a few straight alphas, so a full RGB sweep at alpha 0, 1, 128, and 254 was not part of the enforced set. Each enforced case now has an `-alpha-measure` twin with that sweep. The twins are labeled **measured-not-enforced**: the job prints the mismatch count, the max absolute difference per channel, and the first mismatching pixels (source, GPU, CPU), and a mismatch does not fail CI. They are not written to `tests/parity/cpu-reference.txt`. No shader, formula, or recorded hash changed.

Source texels are straight RGBA. R is `x & 255`, G is `y & 255`, B is `(3x+5y) & 255`, and alpha cycles through 0, 1, 128, and 254. `source` in the report is the straight texel at the output pixel's document position. An identity layer uses that texel. A transformed layer samples elsewhere.

The color-adjust effect pass is a different shader (`rgba16float`) and stays an explicit xfail. This file measures the composite cases only.

`extremes-alpha-measure` pixel (80,2) is source `80,2,250,128` (alpha 128). All three backends match the CPU reference on that pixel and on every other pixel in the sweep.

Counts below are mismatched pixels. The parenthetical is max absolute difference per channel, r,g,b,a. CI run [40577152867](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40577152867) on `4749b61`.

| backend | adapter | CPU reference |
| --- | --- | --- |
| lavapipe | `llvmpipe (LLVM 20.1.2, 256 bits)` on `ubuntu-24.04`, `mesa-vulkan-drivers 25.2.8-0ubuntu0.24.04.4` | Rust `cpu_composite` render version 2 |
| Metal | `Apple Paravirtual device` on `macos-15` (native job) | Rust `cpu_composite` render version 2 |
| SwiftShader | Chrome SwiftShader (`ALTO_BROWSER_GPU=swiftshader`, wasm job) | JavaScript `renderCase` render version 2 |

| case | lavapipe | Metal | SwiftShader |
| --- | ---: | ---: | ---: |
| blend-normal-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-multiply-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-screen-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-overlay-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-darken-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-lighten-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-color_dodge-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-color_burn-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-hard_light-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-soft_light-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-difference-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-exclusion-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-hue-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-saturation-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-color-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-luminosity-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-add-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| opacity-half-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| opacity-zero-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| hidden-layer-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| isolated-group-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| pass-through-group-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| nested-groups-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| translate-across-tile-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| translate-across-tile-1-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| half-pixel-translate-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| scale-bilinear-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| extremes-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| alpha-extremes-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| two-partial-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| eight-layers-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| odd-canvas-0-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| odd-canvas-1-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| level1-centers-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| rotation-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| large-translation-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| large-translation-out-of-range-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |

No backend had a mismatching pixel, so there is no source/GPU/CPU triple to list. The job artifacts still contain `alpha-measure.json` for each backend (`alpha-measure-metal`, `gpu-parity-report`, `alpha-measure-swiftshader`).
