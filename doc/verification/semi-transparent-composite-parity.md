# Semi-transparent v2 composite parity

Render version 2 composite parity (`composite.wgsl` against the CPU reference) locks 37 cases in `tests/parity/cpu-reference.txt`. Several of those cases are opaque, or use only a few straight alphas, so a full RGB sweep at alpha 0, 1, 128, and 254 was not part of the enforced set. Each enforced case has an `-alpha-measure` twin with that sweep. The twins are labeled **measured-not-enforced**: the job prints the mismatch count, the max absolute difference per channel, and the first mismatching pixels (source, GPU, CPU), and a mismatch does not fail CI. They are not written to `tests/parity/cpu-reference.txt`. No shader, formula, or recorded v1/v2 hash changed.

An earlier measurement painted every layer with the same image, so in a two-layer blend the source texel equaled the destination texel. Difference stayed near zero, darken and lighten were identities, and hue, saturation, color, and luminosity were self-composites. That all-zero table did not exercise compositing. It is withdrawn.

Each layer now has its own straight image, in paint order. Layer 0 keeps R = `x & 255`, G = `y & 255`, B = `(3x+5y) & 255`. Layer `i` rotates those channels by `i % 3` and adds `17*i`, so neighboring layers do not share an RGB triple. Alpha is one of 0, 1, 128, 254. Even layers step alpha with `x+y`, odd layers step with `x` only, and both add the layer index as a phase. Layers 0 and 1 therefore hit all 16 `(destination alpha, source alpha)` pairs. The Rust sweep in `crates/alto-gpu/src/cpu_composite.rs` and the JavaScript sweep in `tests/e2e/cpu-composite.js` use that formula. `source` in the report is the straight texel at the output pixel's document position. An identity layer uses that texel. A transformed layer samples elsewhere.

The color-adjust effect pass is a different shader (`rgba16float`) and stays an explicit xfail. This file measures the composite cases only.

## Known limitation

The render version 2 color-adjust mismatch stays for existing version 2 documents that contain color-adjust. At alpha 128 the GPU writes `40,1,125` and the CPU reference writes `40,1,126`. Output of those documents varies by device. Render version 3 (#22) fixes the effect only for new documents and for documents that have been upgraded. The remedy for an old document is `render.upgrade`.

## Measurement

Counts below are mismatched pixels. The parenthetical is max absolute difference per channel, r,g,b,a. The table is filled from the CI run of the commit that contains this sweep. Until that run is recorded here, do not treat a blank cell as a zero.

| backend | adapter | CPU reference |
| --- | --- | --- |
| lavapipe | `llvmpipe` on `ubuntu-24.04` | Rust `cpu_composite` render version 2 |
| Metal | `Apple Paravirtual device` on `macos-15` (native job) | Rust `cpu_composite` render version 2 |
| SwiftShader | Chrome SwiftShader (`ALTO_BROWSER_GPU=swiftshader`, wasm job) | JavaScript `renderCase` render version 2 |

| case | lavapipe | Metal | SwiftShader |
| --- | ---: | ---: | ---: |
| blend-normal-alpha-measure | | | |
| blend-multiply-alpha-measure | | | |
| blend-screen-alpha-measure | | | |
| blend-overlay-alpha-measure | | | |
| blend-darken-alpha-measure | | | |
| blend-lighten-alpha-measure | | | |
| blend-color_dodge-alpha-measure | | | |
| blend-color_burn-alpha-measure | | | |
| blend-hard_light-alpha-measure | | | |
| blend-soft_light-alpha-measure | | | |
| blend-difference-alpha-measure | | | |
| blend-exclusion-alpha-measure | | | |
| blend-hue-alpha-measure | | | |
| blend-saturation-alpha-measure | | | |
| blend-color-alpha-measure | | | |
| blend-luminosity-alpha-measure | | | |
| blend-add-alpha-measure | | | |
| opacity-half-alpha-measure | | | |
| opacity-zero-alpha-measure | | | |
| hidden-layer-alpha-measure | | | |
| isolated-group-alpha-measure | | | |
| pass-through-group-alpha-measure | | | |
| nested-groups-alpha-measure | | | |
| translate-across-tile-alpha-measure | | | |
| translate-across-tile-1-alpha-measure | | | |
| half-pixel-translate-alpha-measure | | | |
| scale-bilinear-alpha-measure | | | |
| extremes-alpha-measure | | | |
| alpha-extremes-alpha-measure | | | |
| two-partial-alpha-measure | | | |
| eight-layers-alpha-measure | | | |
| odd-canvas-0-alpha-measure | | | |
| odd-canvas-1-alpha-measure | | | |
| level1-centers-alpha-measure | | | |
| rotation-alpha-measure | | | |
| large-translation-alpha-measure | | | |
| large-translation-out-of-range-alpha-measure | | | |
