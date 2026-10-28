# Semi-transparent v2 composite parity

Render version 2 composite parity (`composite.wgsl` against the CPU reference) locks 37 cases in `tests/parity/cpu-reference.txt`. Each locked case has an `-alpha-measure` twin. The twins stay **measured-not-enforced**: a mismatch is printed and stored, and it does not fail CI. They are not in `tests/parity/cpu-reference.txt`. No v2 formula and no recorded v1 or v2 hash changed.

Each layer has its own straight image, in paint order. Layer 0 keeps R = `x & 255`, G = `y & 255`, B = `(3x+5y) & 255`. Layer `i` rotates those channels by `i % 3` and adds `17*i`. Alpha is one of 0, 1, 128, 254. Even layers step alpha with `x+y`, odd layers step with `x`, and both add the layer index. Layers 0 and 1 cover all 16 `(destination alpha, source alpha)` pairs, and source RGB differs from destination at every pixel. The Rust sweep and `tests/e2e/cpu-composite.js` use that formula.

## Serious known limitation

Under render version 2, compositing is bit-exact only when at least one of src or dst is opaque or alpha 0. Semi-transparent over semi-transparent can differ by ±1, as measured on lavapipe. In this P2P design that becomes a tile-hash divergence between peers: the same document is different on different peers. v2 is frozen with this known nondeterminism. The remedy is `render.upgrade` to the future `render_version` 3 integer compositing.

There is no exact-CPU-references design memo on main. The same statement is in [IMAGE-EDITING.md](../design/IMAGE-EDITING.md).

Metal does not match that lavapipe split exactly. `opacity-half-alpha-measure` and `nested-groups-alpha-measure` differ on 4098 pixels (max abs `1,0,1,1`) where the bottom alpha is 0 and the top alpha is 1 at layer opacity 1/2. The composited alpha sits on the 0.5/255 boundary. SwiftShader matches the lavapipe case list, plus one `blend-color` pixel.

The render version 2 color-adjust mismatch also stays for existing version 2 documents that contain color-adjust. At alpha 128 the GPU writes `40,1,125` and the CPU reference writes `40,1,126`. Output of those documents varies by device. Render version 3 (#22) fixes that effect only for new documents and for documents that have been upgraded. The remedy for an old document is `render.upgrade`.

## Measurement

Counts are mismatched pixels. The parenthetical is max absolute difference per channel, r,g,b,a. Lavapipe and Metal are from CI run [40586190451](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40586190451) on `0fb35bb` (`gpu-parity` and `native` succeeded). SwiftShader is from CI run [40583754183](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40583754183) on `4ffd834`, the same sweep and the same JavaScript composite. The wasm job on `0fb35bb` stopped in the embed suite with `GPU_LOST: the previous frame did not finish on the GPU` before the parity suite, so it did not record SwiftShader again. No v2 formula changed between those commits.

| backend | adapter | CPU reference |
| --- | --- | --- |
| lavapipe | `llvmpipe (LLVM 20.1.2, 256 bits)` on `ubuntu-24.04`, `mesa-vulkan-drivers 25.2.8-0ubuntu0.24.04.4` | Rust `cpu_composite` render version 2 |
| Metal | `Apple Paravirtual device` on `macos-15` (native job). This is not real Apple hardware; real hardware is unverified | Rust `cpu_composite` render version 2 |
| SwiftShader | Chrome SwiftShader (`ALTO_BROWSER_GPU=swiftshader`, wasm job) | JavaScript `renderCase` render version 2 |

| case | lavapipe | Metal | SwiftShader |
| --- | ---: | ---: | ---: |
| blend-normal-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-multiply-alpha-measure | 5 (1,1,0,0) | 5 (0,1,0,0) | 5 (1,1,0,0) |
| blend-screen-alpha-measure | 1 (1,0,0,0) | 1 (1,0,0,0) | 1 (1,0,0,0) |
| blend-overlay-alpha-measure | 2 (1,0,1,0) | 0 (0,0,0,0) | 2 (1,0,1,0) |
| blend-darken-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-lighten-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-color_dodge-alpha-measure | 1 (1,0,0,0) | 0 (0,0,0,0) | 1 (1,0,0,0) |
| blend-color_burn-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-hard_light-alpha-measure | 2 (1,0,1,0) | 0 (0,0,0,0) | 2 (1,0,1,0) |
| blend-soft_light-alpha-measure | 1 (0,0,1,0) | 0 (0,0,0,0) | 1 (0,0,1,0) |
| blend-difference-alpha-measure | 0 (0,0,0,0) | 1 (0,1,0,0) | 0 (0,0,0,0) |
| blend-exclusion-alpha-measure | 3 (1,1,0,0) | 2 (0,1,0,0) | 3 (1,1,0,0) |
| blend-hue-alpha-measure | 0 (0,0,0,0) | 1 (0,0,1,0) | 0 (0,0,0,0) |
| blend-saturation-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-color-alpha-measure | 0 (0,0,0,0) | 1 (0,0,1,0) | 1 (0,0,1,0) |
| blend-luminosity-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| blend-add-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| opacity-half-alpha-measure | 0 (0,0,0,0) | 4098 (1,0,1,1) | 0 (0,0,0,0) |
| opacity-zero-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| hidden-layer-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| isolated-group-alpha-measure | 1 (1,0,0,0) | 0 (0,0,0,0) | 1 (1,0,0,0) |
| pass-through-group-alpha-measure | 0 (0,0,0,0) | 1 (1,0,0,0) | 0 (0,0,0,0) |
| nested-groups-alpha-measure | 0 (0,0,0,0) | 4098 (1,0,1,1) | 0 (0,0,0,0) |
| translate-across-tile-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| translate-across-tile-1-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| half-pixel-translate-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| scale-bilinear-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| extremes-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| alpha-extremes-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| two-partial-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| eight-layers-alpha-measure | 6 (1,1,0,0) | 9 (1,1,0,0) | 6 (1,1,0,0) |
| odd-canvas-0-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| odd-canvas-1-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| level1-centers-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| rotation-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| large-translation-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |
| large-translation-out-of-range-alpha-measure | 0 (0,0,0,0) | 0 (0,0,0,0) | 0 (0,0,0,0) |

### First mismatch

Lavapipe and SwiftShader agree on these pixels unless noted.

| case | pixel | source | lavapipe GPU / CPU | Metal GPU / CPU | SwiftShader GPU / CPU |
| --- | --- | --- | --- | --- | --- |
| blend-multiply-alpha-measure | lavapipe (93,65); Metal (208,127) | lavapipe `93,65,92,128` \| `82,109,110,128` | `51,51,60,192` / `51,50,60,192` | `207,127,234,254` / `207,126,234,254` | same as lavapipe |
| blend-screen-alpha-measure | (20,143) | `20,143,7,254` \| `160,24,37,1` | `21,142,7,254` / `20,142,7,254` | same | same |
| blend-overlay-alpha-measure | (237,62) | `237,62,253,254` \| `79,14,254,128` | `233,34,254,255` / `233,34,253,255` | 0 pixels | same as lavapipe |
| blend-color_dodge-alpha-measure | (230,1) | `230,1,183,254` \| `18,200,247,254` | `247,5,255,255` / `246,5,255,255` | 0 pixels | same as lavapipe |
| blend-hard_light-alpha-measure | (237,62) | `237,62,253,254` \| `79,14,254,128` | `191,34,254,255` / `191,34,253,255` | 0 pixels | same as lavapipe |
| blend-soft_light-alpha-measure | (242,8) | `242,8,254,128` \| `25,15,3,254` | `129,8,129,255` / `129,8,128,255` | 0 pixels | same as lavapipe |
| blend-difference-alpha-measure | Metal (229,192) | `229,192,111,1` \| `209,128,246,128` | 0 pixels | `105,65,123,128` / `105,64,123,128` | 0 pixels |
| blend-exclusion-alpha-measure | (189,136) | `189,136,223,1` \| `153,240,206,128` | `77,121,104,128` / `77,120,104,128` | same | same |
| blend-hue-alpha-measure | Metal (181,82) | `181,82,185,254` \| `99,202,198,128` | 0 pixels | `105,107,156,255` / `105,107,157,255` | 0 pixels |
| blend-color-alpha-measure | (181,82) | `181,82,185,254` \| `99,202,198,128` | 0 pixels | `105,107,157,255` / `105,107,156,255` | same as Metal |
| opacity-half-alpha-measure | Metal (0,0) | `0,0,0,0` \| `17,17,17,1` | 0 pixels | `0,0,0,1` / `0,0,0,0` | 0 pixels |
| isolated-group-alpha-measure | (230,111) | `230,111,221,1` \| `128,238,247,254` \| `255,8,145,254` | `255,9,145,255` / `254,9,145,255` | 0 pixels | same as lavapipe |
| pass-through-group-alpha-measure | Metal (230,111) | same sources as isolated-group | 0 pixels | `255,9,145,255` / `254,9,145,255` | 0 pixels |
| nested-groups-alpha-measure | Metal (0,0) | `0,0,0,0` \| `17,17,17,1` | 0 pixels | `0,0,0,1` / `0,0,0,0` | 0 pixels |
| eight-layers-alpha-measure | lavapipe (156,112); Metal (110,25) | lavapipe eight texels ending `231,123,19,254` | `231,123,19,255` / `230,123,19,255` | `29,193,110,255` / `29,194,110,255` | same as lavapipe |

## Pre-quantization dump

The parity harness copies `accum` into a storage buffer and then does the same `textureStore` into `rgba8unorm`. The CPU records the same accumulator before `round_ties_even`. Production `composite.wgsl` is unchanged. `gpu-parity-report` contains `prequant.json` and `prequant.md` for every mismatching channel: both f32 values, their raw bits, and the distance of each from the nearest `n+0.5` boundary in byte space (`|channel * 255 - nearest n+0.5|`).

On lavapipe (`llvmpipe (LLVM 20.1.2, 256 bits)`), 22 channels differ:

- 0 of 22 are bit-identical.
- 22 of 22 are within `1e-4` of an `n+0.5` boundary (also within `1e-3`).
- 0 channels have `|GPU - CPU| * 255` above `1e-3`.
- Max `|GPU - CPU| * 255` is `4.55975533e-5` (`blend-color_dodge-alpha-measure` pixel (230,1) red: GPU `0.966666698` / bits `1064793976`, CPU `0.96666652` / bits `1064793973`, bytes 247 vs 246).

The f32 values differ by 1 to 3 ulps. Every one of those gaps sits on a rounding boundary, and that is where the stored byte becomes ±1. The ops in the mismatching cases are unpremultiply division (`dst.rgb / max(dst.a, 0.000001)` inside `blend`, and the isolated-group pop which unpremultiplies the child accumulator the same way) and the blend FMAs (`mix_color` and the three-term sum). The dump does not separate those ops. The largest gap is color dodge, which divides. The other gaps are one or two ulps, including multiply, screen, overlay, hard light, soft light, exclusion, the isolated-group pop, and the eight-layer normal chain.

On Metal in the same run, 8217 channels differ. 8208 of them are bit-identical before quantization and still store a different byte, so that ±1 is the `rgba8unorm` store versus CPU `round_ties_even` on an exact `n+0.5` boundary. The other 9 differ by one ulp (`|GPU - CPU| * 255` at most `1.51991844e-5`): `blend-hue` and `blend-color` blue at (181,82), and seven channels in `eight-layers`. All 8217 are within `1e-4` of an `n+0.5` boundary. None exceed `1e-3`.

Metal exact-half count, [CI run 40593541861](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40593541861) on `30a4437` (`Apple Paravirtual device`, not real Apple hardware; real hardware is unverified): of 8208 bit-identical pre-store channels, 8208 have `v*255` exactly equal to `n+0.5` and 0 do not; 8201 differ as CPU `round_ties_even` versus `round-half-up`, and 7 are exact halves where those two rules agree but the GPU byte is the other integer (all 8208 GPU bytes match round-ties-to-odd).

`rgba8unorm` store versus CPU `round_ties_even` is the step that turns a tie, or a one-ulp gap on that tie, into a byte. It is not a second formula.

## Atlas read (enforced)

Render version 3 keeps the atlas as one `Rgba8Unorm` texture and reads it with `textureLoad` followed by `unorm_byte` (`u32(clamp(v, 0, 1) * 255 + 0.5)`, the `effects.wgsl` function color-adjust mode 18 already uses). The same jobs check that read, and the check is required: a 256x1 `Rgba8Unorm` texture holds every byte value on every channel (R = i, G = 255 − i, B = i ^ 0x55, A = i + 128), a compute shader reads it back, and each backend asserts that all 1024 channels round-trip exactly.

- Metal (native job) and lavapipe (gpu-parity job): `Gpu::atlas_read` in `crates/alto-gpu/src/cpu_composite.rs`, run at the end of `gpu_shader_matches_cpu_reference`. Output `atlas-read.json` and `atlas-read.md` beside `alpha-measure.json`, plus an `atlas-read (enforced): …` log line. `atlas_read_uses_effects_unorm_byte` checks the shader text matches `effects.wgsl`.
- SwiftShader (wasm job): `measureAtlasRead` in `tests/e2e/parity-tests.js`, stored as `atlasRead` in `parity.json` and summarized by `scripts/alpha-measure-summary.mjs`. The `atlas-read round-trips all 256 bytes` test fails the parity run on a mismatch or a GPU error.

Any mismatch or error fails the job; `atlas-read.json` is still written first. A mismatch on some backend is the only reason to switch the atlas to `Rgba8Uint`.
