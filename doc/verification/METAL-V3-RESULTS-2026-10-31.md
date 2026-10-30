# 実機 Metal での v3 一致確認

2026-10-31 JST。対象は main の `f5fa623`(#55 まで)。実機は Apple M5(`system_profiler` の表記は Metal 4、Chrome の WebGPU アダプタの `architecture` は `metal-3`)、macOS 27.2。CI の macOS ランナーは仮想 GPU(Apple Paravirtual device)なので、実機の Metal で v3 の GPU 結果が CPU 参照実装と全バイト一致するかを、リリース前に一度確かめた。

生の結果はこのディレクトリに置かない。

## ネイティブ(wgpu → Metal)

CI の `gpu-parity` ジョブ(lavapipe)と同じテストを、`ALTO_REQUIRE_GPU_PARITY=1` で実行した。アダプタは `Apple M5 (Metal)`。

- `cpu_composite::gpu_shader_matches_cpu_reference`: 通過。
  - v3 の合成 74 ケース(`composite-v3`、すべて強制)で、不一致の画素は 0(`integerExactCount` 74、`v3UnsupportedCount` 0)。合成モード全 17 種、不透明度、非表示レイヤー、isolated / pass-through / 入れ子のグループ、平行移動・拡大縮小・回転・大きな平行移動を含み、うち 37 ケースはレベル 1(縮小ピラミッド)。
  - アトラスの読み取り(`textureLoad` + `unorm_byte`)は、Rgba8Unorm の 4 チャンネルと R8Unorm で、256 通りすべてが正確に往復した。
- v3 のベクター塗り、ベクターマスク、ぼかし(raster / backdrop)、テキスト 13 件(`cpu_composite::v3_vector_` / `v3_blur_` / `v3_raster_blur_` / `v3_backdrop_blur_` / `v3_feathered_vector_mask_` / `v3_adjustment_vector_mask_` / `v3_text_`): すべて Rust の整数参照とバイト一致した。

参考として、強制していない v2 の測定(`alpha-measure`)では、multiply / screen / difference / exclusion / color / pass-through で 1〜5 画素、opacity-half で 4098 画素に ±1 の差が出た。pre-quant の dump では差のあるチャンネルはすべて n+0.5 の境界にあり、GPU の unorm 格納と CPU の `round_ties_even` の違いだった。v3 を整数にした理由そのもので、v2 の挙動は変えていない。

## ブラウザ(Chrome のハードウェア WebGPU → Metal)

`node scripts/verify-browser.mjs --functional` を `ALTO_BROWSER_GPU` なしで実行した。Chrome 155.0.8059.84、アダプタは `{"vendor":"apple","architecture":"metal-3"}`。

| suite | 結果 |
| --- | ---: |
| functional | 36 / 36 |
| m5 | 9 / 9 |
| m6 | 11 / 11 |
| m7 | 9 / 9 |
| m8 | 6 / 6 |
| embed | 37 / 37 |
| parity | 243 / 243(うち v3 156 件) |

## リリース用 WASM の再現性

この Mac でビルドした release パッケージ(`alto_wasm_bg.wasm` と JS / d.ts)は、main CI の `wasm-hashes-main`(run 41143119417)が提案した SHA-256 とバイト単位で一致した。比較用 WASM(`vector-comparison`)も一致した。
