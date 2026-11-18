# 色空間とビット深度

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** sRGB / 8bit 固定を解き、広色域と高精度を段階的に扱う。

**Architecture:** 文書の色の定義は `DocumentMeta`（`crates/alto-doc/src/lib.rs`）の `color`、`depth`、`render_version` にある。型は既にあり、検証が sRGB / U8 / render v3 以外を拒否している。文書の画素を変えずに解釈だけを変える項目（COL-1、COL-5）を先に行う。次に合成の中間を Q30 にする render v4（RV4-1）を作り、その上に画素の格納を変える項目（COL-2、COL-3）と合成の式を変える項目（COL-4）を載せる。CMYK と特色（COL-6）は、編集を RGB のまま、表示と入出力だけで扱う。

**11-04 版の誤りの訂正（全体）。**

- `renderer/m5.rs`、`cpu_composite.rs`、`composite_q30.rs` は今は無い。M5 の段は `crates/alto-gpu/src/renderer/tile_renderer/`（`mod.rs`、`stages.rs`、`paint.rs`、`targets.rs`）、CPU 参照は `crates/alto-gpu/src/cpu_composite/`（`q14.rs`、`q14_stages.rs` など）に分かれた（`4121d69`、`725d6f4`、`03d7081`）。
- v3 の 64bit の組 `Wide` は `crates/alto-gpu/shaders/q14.wgsl`（と `composite-v3-layout.wgsl`）にある。`effects-v3-wide.wgsl` は Q8 の位置の補助（`pixel_q8`、`coord_q8`、`wide_round_shr16`）で、`Wide` を定義しない。
- v3 のベクターと文字は、実行時に `glyph-q14.wgsl` が着色する（`tile_renderer/paint.rs` の `paint_crops`）。`vector-q14.wgsl` は試験（`renderer.rs` の `shader_tests`、`tile_renderer/pool_tests.rs`）だけが読む式の写しである。
- 書き出しは canvas の `convertToBlob` ではない。表示タイルを読み戻し、SDK の PNG 符号化器（`packages/alto-sdk/src/png.ts`、`sRGB` chunk）で出す（`0634560`）。2D canvas を通るのは拡大縮小と WebP だけ。
- wgpu は 11-04 の時点から 30.0.1 で、`SurfaceConfiguration` の `color_space` に `SurfaceColorSpace::DisplayP3` を渡せる。11-04 版の「wgpu 経由で渡せない場合は `present-q14.wgsl` で行列変換」は、ブラウザーでは不要（COL-1）。
- 11-04 版の「COL-5 の変換で別の文書を作る」は、COL-5 の設計に変換が無かった。決定が要る点に移した。

## 決定が要る点

| 点 | 案 | 既定 |
|---|---|---|
| P3 の書き出しの色の印 | A：PNG に cICP（4 バイト）を書く。B：iCCP（Display P3 の行列プロファイル。COL-5 の `alto-color` で作る） | A。ブラウザー試験で Chrome が cICP を読まないと分かれば B にし、COL-1 の書き出しを COL-5 の後へ回す |
| 色空間・深さ・描画版の変換 | A：作らない（書き出して新しい文書へ読み込む）。B：別の文書を作る変換 API（新しい項目になる） | A |
| render v4 を、8bit・ガンマ・`text_rendering`（新規）なしの文書に許すか | A：`renderVersion: 4` を明示したときだけ許す。B：拒否する | A。8bit の文書で v4 の3実装を試験できる |
| 16bit 文書の画像の読み込みの上限 | A：32M 画素（8bit の 64M 画素と同じバイト数）。B：64M 画素のまま | A。RV4-1 の測定で WASM の B6 が収まれば B |
| F16 文書の Normal、Multiply、Add 以外の合成モード | A：各入力を [0, 1] に丸めてから v4 の式。B：モードごとに範囲を広げた式を定義する | A |
| CMYK プロファイルの同梱 | A：同梱しない（試験は合成したプロファイル）。B：再配布できるフリーのプロファイルを同梱し、`THIRD-PARTY-NOTICES.md` に記載する | A |

## 共通の現状

- `crates/alto-core/src/lib.rs`：`ColorSpace { Srgb, DisplayP3 }`、`BitDepth { U8, U16, F16 }`、`PixelFormat { Rgba8, R8 }`（`bytes_per_pixel` は 4 と 1）。`RENDER_VERSION_F32 = 1`、`RENDER_VERSION = 2`、`RENDER_VERSION_EXACT = 3`。
- `crates/alto-doc/src/lib.rs`：`DocumentMeta { width, height, color, depth, format_version, render_version }`（`deny_unknown_fields`）。`DocumentMeta::srgb_u8` が作る。非公開の `validate` は、`format_version != 5` と `render_version != 3` を `UNSUPPORTED`、sRGB / U8 以外を `INVALID_ARGUMENT`（`Only sRGB 8-bit documents are supported`）にする。試験 `meta_rejects_unimplemented_color_and_depth` が P3 と F16 の拒否を固定している。
- 文書の形式番号 5 は複数の箇所に固定で書かれている。`DocumentMeta::srgb_u8` と `validate`（`crates/alto-doc/src/lib.rs`）、`.alto` の manifest の書き出し（`crates/alto-store/src/lib.rs` の `document_format: 5`）と読み込み（`document_format` が 5 以外なら `UNSUPPORTED: package format`）、`crates/alto-engine/src/versions/gc.rs`（manifest の `document_format != 5` を拒否）、`crates/alto-engine/src/versions/merge.rs`（対象の `format_version != 5` を拒否）。
- `Op::SetMeta` はキャンバスの拡縮（`crates/alto-engine/src/canvas.rs`）と `restore_ops`（`crates/alto-doc/src/version.rs`）が使い、`Document::apply` で `validate` を通る。`color`、`depth`、`render_version` を変える要求は今は無いが、`SetMeta` はそれを拒否しない。
- `crates/alto-doc/src/version.rs` の `diff` は `color`、`depth`、`render_version` の差を `MetaChanged` にする。`crates/alto-engine/src/versions/merge.rs` は文書の `root` が違えば `UNRELATED_HISTORY`。
- 作成：`crates/alto-wasm/src/lib.rs` の `Runtime::create_document(w, h, render_version)` は `None | Some(3)` だけを受け、`DocumentMeta::srgb_u8` を使う。辺は 1〜65536（`INVALID_ARGUMENT`）。SDK は `createDocument({ width, height, renderVersion?: 3 })`（`packages/alto-sdk/src/engine.ts`、`protocol.ts`）。`PROTOCOL` は 9。SDK の `DocumentMeta.color` と `depth` は `string`（`packages/alto-sdk/src/types.ts`）。
- 画素の正本：RGBA8 は直線（非乗算）アルファ、R8 はマスクと選択。タイル 256×256。ハッシュは `ContentHash::of_tagged(tag, pixels)` で、tag は 4（RGBA8）か 1（R8）（`crates/alto-raster/src/lib.rs` の `TileData::hash`）。パッケージのオブジェクトは `ALTOOBJ1` + tag + LZ4 で、`crates/alto-store/src/lib.rs` の `object_header` は tag 0、1、4 だけを受ける。大きさが固定なのは tag 4（262144）と tag 1（65536）で、tag 0（一般のバイト列）は 268,435,456 バイトまで任意。復号 Worker（`crates/alto-decode/src/lib.rs` の `decode_batch`）は形式のバイトが 1 なら R8、それ以外を RGBA8 とみなす。
- 描画（render v3）：内容のアトラスは `Rgba8Unorm`、表示タイルは `Rgba8Uint`（乗算済み）。
  - 平易な文書は `integer_composite_shader`（`crates/alto-gpu/src/lib.rs`。`composite-v3-layout.wgsl`、`q14.wgsl` の一部、`composite-v3-fetch.wgsl`、`blend-q14.wgsl`、`composite-q14.wgsl`）。
  - 効果、マスク、クリップ、ベクター、文字のある文書は `tile_renderer` の段（`integer_effects_shader`：`q14.wgsl`、`blend-q14.wgsl`、`effects-v3-wide.wgsl`、`effects-q14.wgsl`）。中間は `rgba16uint` の乗算済み Q14（`target_format(exact)`）。`effects-resolve-q14.wgsl` が `rgba8uint` に戻す。使用中のモードは 8、17〜33。
  - ベクターと文字は、CPU の整数ラスタライザ（`crates/alto-gpu/src/raster_q8.rs`、`glyph_q8.rs`）の被覆を `glyph-q14.wgsl` が塗る。
  - ピラミッドは `downsample-q14.wgsl`、提示は `present-q14.wgsl`（f32、表示だけ。ハッシュの外）。
  - `v3_shader_sources_are_pinned` が、組み立てた2つのシェーダーのハッシュを固定する。
- 版の分岐：`render_version` を見る箇所は `>= RENDER_VERSION_EXACT` と `== 3` が混在する。`crates/alto-engine/src/lib.rs`（文字の `RContent`）、`crates/alto-engine/src/text.rs`、`crates/alto-gpu/src/renderer/compose.rs`、`renderer/scene.rs` の `prepare_scene`、`renderer/tile_renderer/mod.rs` の `compose_tiles`、`renderer/uniform.rs` の `uniform_subtree`、`crates/alto-scene/src/damage.rs`、`crates/alto-gpu/src/sample.rs` の `pack_inverse`、`renderer/geometry.rs` の `WorldTransform::root`（引数名は `version`。`== 3` のときだけ整数の変換 `V3`、ほかは f64 の `Legacy`）、`crates/alto-wasm/src/lib.rs` のストロークの変換（`fixed_world`、`== 3` のときだけ `FixedAffine`）。描く経路の選択（`compose.rs` が `damage::plain_scene` で平易な合成器か `compose_tiles` かを決める）は版を見ず、機能だけを見る。v3 で整数の定義が無い機能は `renderer/geometry.rs` の `v3_gap` が `GPU_UNSUPPORTED` にする。
- 描画の CPU 参照：`crates/alto-gpu/src/cpu_composite/`（`q14.rs`、`q14_stages.rs`、`color_adjust.rs`、`mask_programs.rs`、`reference.rs`）。JS の参照は `tests/e2e/cpu-composite.js`（Q24 の和は BigInt）と、Rust の試験（`reference_tests.rs`）が node で呼ぶ `tests/e2e/composite-v3-reference.mjs`、`m5-v3-renderer-reference.mjs`、`mask-v3-reference.mjs`、`mask-v3-origin-reference.mjs`。固定は `tests/parity/v3-browser.txt`（ブラウザー）と Rust の `cases.rs`。
- 製品の CPU 経路にも v3 の式がある。`Rasterize` は `raster_q8::vector_pixels_q14`（`crates/alto-wasm/src/image_versions.rs` の `layer_edit`）。`MergeDown` は `canvas.rs` の `composite_into`（RGBA8 だけ、独自の Normal）。画像編集（`crates/alto-engine/src/image/`）は RGBA8 だけを扱う。グラデーションは `image/pixels.rs` の `linear`、`srgb`（f64）で補間し、文書の合成とは無関係。
- ブラシは `brush.wgsl` の f32 スタンプ（出力 `rgba8unorm`）と読み戻しが正本で、共通規則2の外（03-brush の冒頭）。
- 文字は焼き込み（`crates/alto-core/src/text_bake.rs`、ALTB）だけから描く（`tile_renderer/paint.rs` の `text_q14`）。焼き込みは色と精度に依らない。
- 色の入力：`types.ts` の `Color` の注釈は「Linear RGBA」だが、v3 は値をそのまま Q14 にし（`raster_q8::color_q14`）、ガンマのまま合成する。実際は文書の色空間の符号化値である。`image-types.ts` の `ImageColor` の注釈は「Straight sRGB channels」。
- 読み込み：`engine.assets.importImage`（`engine.ts`）が `createImageBitmap(file)` と 2D canvas の `getImageData` で sRGB の RGBA8 にし、Worker の `importRgba`（`Runtime::import_rgba`）がタイルにする。元のファイルは `AssetKind::EncodedImage { mime }` として残る。上限は辺 32768、67,108,864 画素。
- 書き出し：`DocumentHandle.exportImage`（`packages/alto-sdk/src/document.ts`）→ Worker の `exportImage`（`worker.ts`）。表示タイルを `readTiles`（1タイル 262144 バイト）で読み、非乗算にして `encodePng`（8bit RGBA、`sRGB` chunk、Paeth）。拡大縮小と WebP は 2D canvas。64M 画素、または CPU 側の作業量 512 MiB を超えると `INVALID_ARGUMENT`。契約は `doc/design/EXPORT-CONTRACT.md`。
- 提示の面：`renderer/device.rs` が `get_default_config` の非 sRGB 形式で面を作る。`color_space` は既定のまま（`SurfaceColorSpace::Auto`。WebGPU では sRGB）。wgpu 30.0.1 の web backend は、どの形式にも sRGB と Display P3 の canvas を報告し、`Rgba16Float` には拡張範囲の2つも報告する。画面の色域（CSS の `color-gamut`）は `Window` の `matchMedia` でしか分からず、エンジンの Worker の中では取れない。
- メモリの会計（`doc/verification/ENGINE-HARDENING-RESULTS.md` の E-2、`tests/e2e/browser-tests.js` の B6）は、WASM メモリ（`wasmBytes`、復号プールは別に `decodePeakBytes`）、GPU の計上値（`gpuBytes`）、メインスレッドの JS heap を別々に記録する。GPU の計上値は、エンジンが数えた確保量で、物理 VRAM の使用量ではない。
  - GPU の計上値の上限は `createEngine({ gpu: { memoryBudgetMB } })`（既定 512）。`Governor`（`crates/alto-gpu/src/renderer/memory.rs`）が超える確保を `RenderError::Budget`（コードは `INVALID_ARGUMENT`）にする。表示タイルとアトラスの層数は `cache_layers` が予算の半分を `TILE_RGBA_BYTES`（256×256×4）で割って決める。`gpu_bytes` の `live_fixed` も同じ定数で数える。
  - B6 の基準は、GPU の計上値 512 MiB 以下、WASM と復号プールの和 1024 MiB 以下。E-2 の 4K・20層では GPU の計上値の最大 493 MiB、WASM 184 MiB。
  - CPU 側の復号キャッシュは 128 MiB の LRU、`.alto` は 512 MiB が上限（`doc/MILESTONES.md`「上限・継続課題」6）。

## 共通の設計：形式番号を上げる経路

`format_version` を上げる PR のうち、全文書で最初にマージされるもの（計画では回 6。06 の TXT-4〜TXT-7）が次をまとめて変える。この文書の項目（RV4-1、COL-2、COL-4、COL-3、COL-6）は、その後に番号を足すだけである。文書の型だけを上げると、`.alto` に保存して開けない（manifest の `document_format` が 5 に固定され、読み込みが 5 以外を拒否するため）。

- `alto_doc::FORMAT_VERSION`（新規）（現在の値）と `alto_doc::SUPPORTED_FORMATS`（読める値の組。5 と新しい値）を置き、上に挙げた固定の 5 をすべてこれに置き換える。
- 文書は、新しい形式の意味（`render_version` 4、`Rgba16`、`blend_space`（新規）など）を使うときだけ新しい番号で書く。番号は、文書が使う意味のうち最も新しい回の番号で、`Document::required_format`（新規）が求め、スナップショットを書くときに `DocumentMeta.format_version` に入れる。使わない文書は 5 のまま書き、今の読み手でも開ける（共通規則3の「最初のリリースまでは移行を作らない」に従い、5 の文書を新しい番号へ書き換える処理は作らない）。
- manifest の `document_format` は、保存するスナップショットの `format_version` を書く（`manifest_bytes` と `manifest_bytes_with_refs` の引数に足す）。読み込みと `gc.rs` は `SUPPORTED_FORMATS`（新規）に含まれれば受ける。manifest 自体の `format` はこの経路では変えない（2 / 3。08 の VER-4 が 4 を足す）。
- `merge.rs` の検査は「両方の版の `format_version` が `SUPPORTED_FORMATS`（新規）に含まれる」にする。片方だけが新しい意味を使うマージ（例：片方が 5、片方が 6）は断らない。結果の番号は `Document::required_format`（新規）で求め直す。試験 `merge_of_format_5_and_6_writes_6`（新規）。
- 試験（新規）：`new_format_document_saves_and_reopens`（`.alto` とフォルダ、版の外置き）、`format_5_documents_still_save_as_5`、`unknown_document_format_is_unsupported`。
- 値は 00-ROADMAP の番号の表で決めた。この基盤は最初に番号を上げる PR（回 6、06 の TXT-4〜TXT-7）が入れる。本書の項目は RV4-1・COL-2・COL-4 と 06 の TXT-10 が 13、COL-3 が 14、COL-6 が 15。RV4-1 は基盤を作らず、`SUPPORTED_FORMATS` に 13 を足す。（新規）

## COL-1 Display P3

### 現状

- `DocumentMeta::validate` が P3 を拒否する（共通の現状）。色空間を選ぶ API は SDK にも WASM にも無い。
- 表示の面は sRGB。P3 の文書を作れないので、変換は要らなかった。
- 書き出しの PNG は常に `sRGB` chunk（`packages/alto-sdk/src/png.ts` の `encodePng`）。拡大縮小と WebP の canvas は既定の色空間（sRGB）。
- 読み込みはブラウザーが sRGB へ変換した画素を受ける（`engine.ts` の `assets.importImage`）。
- `Color` と `ImageColor` の注釈は sRGB や線形を前提に書かれている。

### 11-04 からの変化

- `crates/alto-doc/src/lib.rs` は `126f2b5` でモジュールに分かれたが、`DocumentMeta` と `validate` の内容は変化なし。
- 書き出しが SDK の PNG 符号化に変わった（`0634560`）。11-04 版の「`convertToBlob` で `display-p3` の canvas から出す」は PNG の等倍の経路に合わない。PNG は chunk で色を示す形に直した。
- 11-04 版の「wgpu 経由で渡せない場合」の代替は、wgpu 30.0.1 が Display P3 の canvas を渡せるので不要（ネイティブの面だけの代替に縮めた）。
- 11-04 版の「`engine.status` の警告」は、`EngineStatus` が `state` と `error` だけで警告を持たないので、表示の情報を別に持つ形に直した。

### 設計

P3 の文書は、同じバイト列を P3 の符号化値として解釈する。合成の整数演算は色空間に依らないので、render v3 のタイルの画素とハッシュは sRGB の文書と同じになる。変わるのは表示、読み込み、書き出し、色の入力の解釈だけ。

**文書。**

- `DocumentMeta::validate` が `(Srgb | DisplayP3, U8)` を許す。`render_version` は 3 のまま。`U16`、`F16` は従来どおり `INVALID_ARGUMENT`（COL-2、COL-3 まで）。
- `DocumentMeta::new(width, height, color, depth, render_version) -> DocumentMeta`（新規）を足す。`srgb_u8` はその別名として残す。
- 不変の検査（新規）：`Document::apply` の `Op::SetMeta` で、`color`、`depth`、`render_version` のどれかが今の値と違えば `INVALID_ARGUMENT`（`color model cannot change`）。比べるのはこの3つ（と後で足す `blend_space`）だけで、幅、高さ、`format_version` は従来どおり変えられる。キャンバスの拡縮はこの3つを変えない。`restore_ops` は `meta` が違えば `SetMeta` を出すが、同じ文書（同じ `root`）の版はすべて作成時の3つの値を持つ（この検査がそれを保つ）ので、戻すのは幅と高さ（と `format_version`）だけになる。3つが違う版へ戻すのは壊れた履歴だけで、その戻しは `INVALID_ARGUMENT` で失敗し、文書は変わらない（試験 `restore_to_an_older_size_still_works`、`restore_to_a_commit_with_another_color_model_fails`）。RV4-1、COL-4 はこの検査に `blend_space` を足す。
- マージ（新規の検査）：`merge.rs` の計画で、ours と theirs の `meta` の `color`、`depth`、`render_version` が違えば `UNRELATED_HISTORY`（`different color model`）。同じ `root` で違うのは壊れた履歴だけだが、黙って片方を採らない。

**作成の要求。** 引数を JSON の選択肢にまとめ、COL-2、COL-4、TXT-10 で引数の列を変えずに足せるようにする。

```rust
// crates/alto-wasm/src/lib.rs（新規の型）
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateOptions {
    render_version: Option<u32>,   // 既定 3
    color: Option<ColorSpace>,     // "srgb" | "display_p3"、既定 srgb
}
pub fn create_document(&mut self, w: u32, h: u32, options: &str) -> Result<String, JsValue>
```

SDK（新規の引数）：`createDocument({ width, height, renderVersion?: 3, colorSpace?: "srgb" | "display-p3" })`。SDK の `DocumentMeta.color` を `"srgb" | "display_p3"` に狭める。

**表示。**

- `Renderer::set_surface_color(color: ColorSpace) -> Result<()>`（新規）が、面の設定の `color_space` を `SurfaceColorSpace::Srgb` か `SurfaceColorSpace::DisplayP3` にして `configure` し直す。Worker は文書を作る、開く、復元する、GPU を失って作り直すたびに、文書の `color` で呼ぶ。
- 面の能力（wgpu の format_capabilities）に Display P3 が無いとき（ネイティブの一部）は sRGB の面のまま、提示の段で P3→sRGB の行列を掛けて範囲外を切る。シェーダーは `present-p3-srgb.wgsl`（新規、f32、表示だけ）で、`present-q14.wgsl` は変えない。
- 警告：主スレッドの SDK が `matchMedia("(color-gamut: p3)")` と `"(color-gamut: rec2020)"` を見る。結果を `engine.display`（新規）に置き、変わるたびに `display` イベント（新規）を出す。

```ts
// packages/alto-sdk/src/engine.ts（新規の型）
export interface DisplayColor {
  /** The open document's color space. */
  readonly document: "srgb" | "display-p3" | null;
  /** What the canvas was configured with. */
  readonly canvas: "srgb" | "display-p3";
  /** CSS color-gamut of the screen, or null if the browser does not say. */
  readonly gamut: "srgb" | "p3" | "rec2020" | null;
  /** Colors outside the screen's gamut are clipped. */
  readonly clipped: boolean;
}
```

`clipped` は、文書が P3 で、`gamut` が `srgb` か、canvas が sRGB（行列変換の代替）のとき true。`matchMedia` の `change` でも更新する。文書の状態ではないので `snapshot.warnings` には入れない。黙って色がずれることは無い：P3 の canvas はブラウザーが画面へ色を管理し、代替の経路は行列で変換する。

**読み込み。**

- P3 の文書では、`importImage` が `getContext("2d", { colorSpace: "display-p3", willReadFrequently: true })` の canvas に描き、`getImageData(0, 0, w, h, { colorSpace: "display-p3" })` で取り出す。ブラウザーが埋め込みプロファイル（無ければ sRGB とみなす）から P3 へ変換する。sRGB の画像のバイトをそのまま P3 と解釈しない。
- 変換はブラウザーに依る。結果はタイルとして文書に入るので、文書の画素の決定性は保たれる（同じファイルを別のブラウザーで読み込むと違うタイルになりうる。COL-5 で Rust の変換に置き換える）。

**書き出し。**

- `encodePng(pixels, width, height, color)`（新規の引数 `color`）。sRGB は今のまま `sRGB` chunk。P3 は `sRGB` chunk を書かず、`IHDR` の直後に cICP（色域 12＝P3-D65、伝達 13＝sRGB、行列 0、全範囲 1）を書く（決定が要る点）。
- 拡大縮小と WebP は、`OffscreenCanvas` の `getContext("2d", { colorSpace: "display-p3" })` と `new ImageData(pixels, w, h, { colorSpace: "display-p3" })` で描いてから `convertToBlob` する。ブラウザーが色の印を埋め込むかは、出力のファイルを読んで確かめる（PNG は cICP か iCCP、WebP は RIFF の ICCP chunk）。埋め込まない場合は、その形式の P3 の書き出しを `UNSUPPORTED` にする。
- 書き出しの色の印は、画素の比較だけでなくファイルの構造で確かめる。復号した画素の比較はブラウザーが印を解釈することの確認で、印の有無の証明ではない。

**色の入力。**

- `Color`（`types.ts`）と `ImageColor`（`image-types.ts`）は「文書の色空間の符号化値、直線アルファ、0〜1」と注釈を直す。`pickColor` の `color` も同じ。
- SDK に `convertColor(color: Color, from: "srgb" | "display-p3", to: "srgb" | "display-p3", options?: { clip?: boolean }): Color`（新規、純 TS）を足す。TRC を外し、D65 の 3×3 行列（CSS Color 4 の値）を掛け、TRC を戻す。`clip` の既定は true（範囲外の値は v3 の量子化で拒否されるため）。計算は利用者側の入力で、文書には結果の値が入るだけなので、文書の画素の決定性に関わらない。

**上限とエラー。**

- 既存の文書の色空間を変える操作は作らない（決定が要る点）。
- 不明な `colorSpace` と `color` は `INVALID_ARGUMENT`。`renderVersion` が 3 以外は今のまま `INVALID_ARGUMENT: render version must be 3`（RV4-1 で 4 を足す）。

### 形式と通信

- `format_version`：上げない（既定）。`color: "display_p3"` は形式 5 の型に既にある値で、今の読み手は `INVALID_ARGUMENT` で拒否するので誤って読まない。リリース済みの形式も無い。「意味を増やす変更」とみなすなら、RV4-1 の回 13 にまとめる（00-ROADMAP の番号の表は、上げない既定で書いた）。
- `PROTOCOL`：上げる（`createDocument` の引数。11。段階3の回で、COL-5 と同じ番号）。README と RELEASING を同じ PR で更新する。
- render v3 の画素：変えない。`tests/parity/*.txt` は不変。
- シェーダーのモード番号：不要（`present-p3-srgb.wgsl` は提示の段で、効果のモードではない）。
- manifest：触れない。

### 作業手順

**Files:**
- Modify `crates/alto-doc/src/lib.rs`（`validate`、`DocumentMeta::new`）、`crates/alto-doc/src/document.rs`（`Op::SetMeta` の不変の検査）、`crates/alto-engine/src/versions/merge.rs`。
- Modify `crates/alto-wasm/src/lib.rs`（`create_document` の `CreateOptions`）、`crates/alto-gpu/src/renderer/device.rs`（`set_surface_color`）、`crates/alto-gpu/src/renderer.rs`（`shader_tests` に新しいシェーダー）。Create `crates/alto-gpu/shaders/present-p3-srgb.wgsl`（新規）。
- Modify `packages/alto-sdk/src/engine.ts`（`createDocument`、`assets.importImage`、`display`）、`worker.ts`（`createDocument`、`exportImage`、面の設定）、`png.ts`、`types.ts`、`image-types.ts`、`protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`packages/alto-sdk/src/testing.ts`（画面の色域を差し替える試験用の入口）、`doc/design/EXPORT-CONTRACT.md`（P3 の文書は sRGB chunk の代わりに cICP、表示タイルは文書の色空間）。
- Modify `packages/alto-sdk/test/png.test.js`、`tests/e2e/image-version-tests.js`、`crates/alto-engine/src/versions_tests.rs`。

**試験：**
- [ ] `crates/alto-doc/src/lib.rs`：`display_p3_u8_validates_and_deep_does_not`。P3 / U8 は成功、P3 / U16 と sRGB / F16 は `INVALID_ARGUMENT`。`meta_rejects_unimplemented_color_and_depth` を置き換える。
- [ ] `crates/alto-doc/src/document.rs`：`color_model_cannot_change_through_set_meta`。`color` を変える `SetMeta` は失敗し、文書は変わらない。幅だけを変える `SetMeta` は成功する。
- [ ] `crates/alto-engine`：`p3_document_tiles_hash_like_srgb`。同じ操作列の P3 と sRGB の文書で、全タイルのハッシュが等しく、`diff` は `MetaChanged { fields: ["color"] }` だけ。
- [ ] `versions_tests.rs`：`merge_refuses_a_different_color_model`。片方の版の `meta.color` を書き換えた履歴で、計画が `UNRELATED_HISTORY`。
- [ ] `png.test.js`：`display-p3 png has cICP after IHDR and no sRGB chunk`、`srgb png is unchanged`（今の出力と全バイト一致）。
- [ ] SDK の単体試験：`convertColor maps srgb red to known p3`。sRGB (1, 0, 0) が P3 (0.9175, 0.2003, 0.1386) ±0.0005、往復が ±1e-6。
- [ ] ブラウザー試験（`image-version-tests.js`）：`p3 document configures a display-p3 canvas`（`engine.display.canvas`）。`p3 png decodes as display-p3`：P3 (200, 100, 50) で塗った文書を書き出し、`getImageData` を `display-p3` で読むと ±1、sRGB で読むといずれかのチャネルが 5 以上違う。`p3 webp and scaled export keep the color space`：出力のバイトを読み、PNG に cICP（12、13、0、1）があり sRGB chunk が無いこと、WebP に ICCP chunk があり、その rXYZ・gXYZ・bXYZ が Display P3 の値（±2/65536）であることを確かめてから、画素を同じ方法で比べる。`srgb image imported into a p3 document is converted`。`p3 on an srgb screen reports clipped`（試験用の入口で色域を `srgb` にする）。
- [ ] 既存の parity が変わらない（`git diff tests/parity` が空）。

**完了条件：** 上の試験が通る。sRGB の文書の書き出しと画素が変わらない。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。
- 後に続く項目：COL-5（読み込みを Rust の変換に置き換え、iCCP を書けるようにする）、RV4-1（`CreateOptions`（新規）、`DocumentMeta::new`、不変の検査を共有）、COL-6（ソフトプルーフの表示経路）。

## COL-5 ICC カラーマネジメント

### 現状

- ICC を読む処理は無い。`crates/alto-color`（新規）はまだ無い。
- 読み込みはブラウザーの変換に任せている（COL-1 の後は文書の色空間へ、今は sRGB へ）。元のファイルは `AssetKind::EncodedImage` に残るので、後から変換し直せる。
- `png`（純 Rust）と `fdeflate` は `Cargo.lock` に間接依存としてあるが、どのクレートも直接使っていない。07-vector-svg の VEC-3 が `png` を `alto-engine` に足す設計である。JPEG と WebP の構文を読む処理は無い。
- QA-1 のファザーは実装済みで、対象は `fuzz/fuzz_targets/` の commit、font、journal、package、path、recovery、snapshot、svg。

### 11-04 からの変化

- ICC の経路は変化なし（無いまま）。
- 11-04 版は「演算は決定的な Q16」とだけ書いた。TRC の評価、行列の逆、逆 TRC の作り方を、整数だけで決まる形に具体化した。`powf` は環境で結果が変わりうるので使わない。
- 11-04 版の「QA-1 のファザーに追加」は、QA-1 が実装済みになったので、既存の `fuzz/` に対象を足す手順にした。

### 設計

読み込み時に埋め込みのプロファイルで文書の色空間へ変換する。書き出し時に文書の色空間の標準プロファイルを示す。D5 の決定により ICC の処理は自前で書き、新しい依存を足さない。行列と TRC（rXYZ、gXYZ、bXYZ、rTRC、gTRC、bTRC、グレーの kTRC）をこの項目で、LUT（`A2B0`、`B2A0` の `mft1`、`mft2`、`mAB `、`mBA `）を COL-6 で扱う。

**新規 crate `crates/alto-color`（新規）。**

```rust
pub const MAX_PROFILE_BYTES: usize = 4 << 20;   // 4 MiB
pub const MAX_TAGS: usize = 128;
pub const MAX_CURVE_POINTS: usize = 65536;

pub struct Profile {
    pub class: ProfileClass,      // Input | Display | Output | ColorSpace（他は Unsupported）
    pub space: DataSpace,         // Rgb | Gray（COL-6 で Cmyk）
    pub description: String,      // desc / mluc の最初の英語、無ければ空
    pub model: ColorModel,
}
pub enum ColorModel {
    Matrix { matrix: [[i32; 3]; 3], trc: [Trc; 3] },  // s15Fixed16（D50 の PCS）
    Gray { trc: Trc },
    // COL-6：Lut { a2b: Option<Lut>, b2a: Option<Lut> }
}
pub enum Trc { Identity, Gamma(u16), Table(Vec<u16>), Parametric { kind: u8, params: [i32; 7] } }
pub enum Intent { Perceptual, Relative }
pub enum ColorError { Truncated, Malformed(&'static str), Unsupported(&'static str), TooLarge }

pub fn parse_profile(bytes: &[u8]) -> Result<Profile, ColorError>;
pub fn srgb() -> Profile;        // 内蔵。ICC の sRGB と同じ行列（D50 に Bradford で順応済み）と para 型 3
pub fn display_p3() -> Profile;  // 内蔵。P3 の行列と sRGB の TRC
pub fn transform(from: &Profile, to: &Profile, intent: Intent) -> Result<Transform, ColorError>;
impl Transform { pub fn apply_rgba8(&self, pixels: &mut [u8]); }   // アルファは触らない
pub fn convert_rgba8(src: &[u8], from: &Profile, to: &Profile, intent: Intent) -> Result<Vec<u8>, ColorError>;
pub fn write_matrix_profile(profile: &Profile) -> Vec<u8>;          // 書き出しの iCCP 用
pub fn embedded_profile(encoded: &[u8], mime: &str) -> Embedded;    // 下記
```

`ColorError`（新規）の表示は `INVALID_ARGUMENT: icc <詳細>`（`Truncated`、`Malformed`、`TooLarge`）と `UNSUPPORTED: icc <詳細>`（`Unsupported`）。

**読み取り（`parse_profile`（新規））。**

1. 長さが 132 以上、ヘッダーの大きさの欄が `bytes.len()` と等しく、`MAX_PROFILE_BYTES`（新規）以下。署名 `acsp`。違えば `Truncated` か `Malformed`。
2. タグ表は `MAX_TAGS`（新規）以下。各タグの位置と大きさが範囲内（重なりと共有は許す）。
3. 種別：`mntr`、`scnr`、`spac`、`prtr` だけ（`link`、`abst`、`nmcl` は `Unsupported`）。データ空間 `RGB `、`GRAY`（`CMYK` は COL-6）。PCS が `XYZ ` で行列のタグがそろえば `Matrix`。行列が無く LUT だけなら `Unsupported("lut profiles need COL-6")`。
4. `curv` の点数 0 は恒等、1 は u8Fixed8 のガンマ、2 以上は表（`MAX_CURVE_POINTS`（新規）まで）。`para` は型 0〜4。

**変換（整数だけ）。**

1. 入力の表：元の TRC を 256 点で評価し、線形の値を Q24（`u32`）で持つ。表の TRC は隣の2点の線形補間（四捨五入）。ガンマと `para` は `fixed_pow(x_q24, g_q16)`（新規）で計算する。`fixed_pow` は `exp2(g × log2(x))` を、整数の `log2`（先頭の0の数と 256 点の表の補間）と `exp2`（256 点の表の補間）で求める。表は crate の定数で、浮動小数を使わない。
2. 行列：`M = to.matrix⁻¹ × from.matrix`。逆行列は余因子と行列式を `i128` で求め、Q24 に四捨五入（0 から遠い方へ）。行列式が 0 なら `Malformed("singular matrix")`。
3. 適用：`lin_out = M × lin_in`（`i64`）を [0, 2^24] に切る。出力のバイトは、目的の TRC の 256 点の順方向表（手順1と同じ作り方）を二分探索し、線形の値が最も近いバイトを選ぶ（同点は小さい方）。逆関数の `pow` を使わない。
4. 同じプロファイル（行列と TRC が等しい）どうしは変換を省き、元のバイトを返す。sRGB の文書へ未タグの画像を読むと、今と同じバイトになる。
5. グレー：`Gray` から RGB へは `lin = TRC(g)` を3チャネルに入れる（無彩色）。
6. 意図：行列のプロファイルは測色的なので `Perceptual` と `Relative` は同じ結果。黒点補償はしない。

**埋め込みの抽出（`embedded_profile`（新規））。** 結果は `Embedded::{ None, Builtin(Profile), Icc(Vec<u8>), Invalid(&'static str) }`。

- PNG：iCCP（zlib で圧縮）を展開する。展開後が `MAX_PROFILE_BYTES`（新規）を超えたら打ち切って `Invalid`。展開器は VEC-3 が足す `png` crate（07）か、`fdeflate` の直接利用（00-ROADMAP の重なりの表：直接依存の `png` は VEC-3 が足し、本項目は共有する。展開器もそのとき決める）。cICP が色域 12・伝達 13 なら `display_p3()`、色域 1・伝達 13 か `sRGB` chunk なら `srgb()`。gAMA と cHRM だけのものは `None`（sRGB とみなす）。
- JPEG：`APP2` の `ICC_PROFILE\0` の断片を番号順につなぐ。番号の欠けと重複は `Invalid`。
- WebP：RIFF の `ICCP` chunk（非圧縮）。
- そのほか（GIF、BMP、AVIF など）：`None`。

**読み込みの経路の変更。**

- SDK の `importImage` は `createImageBitmap(file, { colorSpaceConversion: "none" })` で、ブラウザーの色変換を止めた符号化値を sRGB の canvas から取り出す（ブラウザー試験で、P3 タグ付き PNG の画素が元の値のまま出ることを確かめる）。
- Worker の `importRgba` は、`encoded` と `mime` から `embedded_profile`（新規）を取り、文書の色空間（`srgb()` か `display_p3()`）へ `convert_rgba8` してからタイルにする。変換は Rust で決定的になる（JPEG の復号自体はブラウザーに依るまま）。
- 結果に `profile` を足す：`ImageAsset.profile: { name: string; converted: boolean; reason: "unsupported" | "invalid" | null } | null`（新規）。`null` はプロファイルが無く sRGB とみなした場合。LUT だけのプロファイルや壊れたプロファイルは、読み込みを止めず `converted: false` と理由を返す（ブラウザーも壊れたプロファイルを無視する）。

**書き出し。** sRGB の文書は今のまま `sRGB` chunk。P3 の文書は COL-1 の cICP に加え、`write_matrix_profile(&display_p3())` を iCCP に入れる（決定が要る点の案 B をここで満たす）。

**上限とエラー。** プロファイル 4 MiB、タグ 128、曲線 65536 点。`parse_profile`（新規）を直接呼ぶ経路は無い（読み込みの中だけ）ので、利用者に届くのは `profile.reason` である。

### 形式と通信

- `format_version`：上げない。文書の型は変わらず、変換後の画素が入るだけ。
- `PROTOCOL`：上げる（`import` の応答に `profile`。11。COL-1 と同じ段階3の回）。
- render v3 の画素：触れない。
- 依存：`alto-color` は新しいワークスペースのメンバー。PNG の展開器の扱いは 07 と揃える。

### 作業手順

**Files:**
- Create `crates/alto-color/Cargo.toml`（新規）、`crates/alto-color/src/lib.rs`（新規）、`parse.rs`、`fixed.rs`、`transform.rs`、`embedded.rs`（いずれも新規）、`fuzz/fuzz_targets/icc.rs`（新規）、`fuzz/seeds/icc/`（新規。sRGB、P3、グレー、`para` 型 0〜4 の小さなプロファイル）。
- Modify `Cargo.toml`（members）、`crates/alto-wasm/Cargo.toml`、`crates/alto-wasm/src/lib.rs`（`import_rgba`）、`fuzz/Cargo.toml`、`fuzz/run-all.sh`。
- Modify `packages/alto-sdk/src/engine.ts`（`assets.importImage`）、`types.ts`（`ImageAsset.profile`）、`png.ts`（iCCP）、`protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`tests/e2e/image-version-tests.js`。

**インターフェース：** Produces `parse_profile`、`transform`、`convert_rgba8`、`embedded_profile`、`write_matrix_profile`、`srgb`、`display_p3`。

**試験（`crates/alto-color`（新規））：**
- [ ] `srgb_to_srgb_is_identity`：各チャネル 17 刻みの 16³ 点と、全 256 の灰色で入出力が一致。
- [ ] `srgb_red_in_p3_is_known`：sRGB (255, 0, 0) が P3 (234, 51, 35) ±1。
- [ ] `p3_to_srgb_matches_an_f64_reference_within_one`：試験の中だけの f64 の参照と比べ、全標本で差が 1 以下。
- [ ] `fixed_pow_error_is_below_2_pow_minus_20`：x を 4096 点、g を 1.8、2.2、2.4、1/2.4 で f64 と比べる。
- [ ] `parametric_curves_0_to_4_are_monotonic`、`curve_table_interpolates`。
- [ ] `lut_only_profile_is_unsupported_until_col6`、`truncated_tag_is_refused`、`tag_outside_profile_is_refused`、`size_field_must_match`、`profile_over_4_mib_is_refused`、`singular_matrix_is_refused`。
- [ ] `written_p3_profile_parses_back_to_the_builtin`。
- [ ] `jpeg_icc_segments_join_in_order`、`jpeg_icc_segment_gap_is_invalid`、`png_iccp_inflates_within_the_limit`、`png_iccp_over_the_limit_is_invalid`、`png_cicp_p3_selects_the_builtin`、`webp_iccp_chunk_is_read`。

**試験（ブラウザー）：**
- [ ] `untagged png imports byte-identical into an srgb document`（COL-5 の前と全バイト一致）。
- [ ] `colorSpaceConversion none returns the encoded bytes`。
- [ ] `p3-tagged png converts into an srgb document and reports it`（`profile.converted` が true、画素が f64 の参照 ±1）。
- [ ] `a broken profile imports unconverted with a reason`。

**ファザー：** `icc` の対象は `parse_profile` と `embedded_profile` を呼ぶ。`fuzz/run-all.sh` に足し、QA-1 と同じ時間で落ちないこと。

**完了条件：** 上の試験とファザーが通る。未タグの画像の読み込み結果が変わらない。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`sh fuzz/run-all.sh`。

### 依存

- 先に要る項目：COL-1（文書の色空間）、QA-1（実装済み。ファザーの枠）。
- 後に続く項目：COL-6（LUT、CMYK）、COL-2（16bit の変換 `apply_rgba16`（新規））。
- 同じものを使う項目：VEC-3（07。PNG の展開器）。直接依存の `png` crate は、先に入る VEC-3（段階2）が足し、本項目は共有する（00-ROADMAP の重なりの表）。

## RV4-1 render v4 の基盤

### 現状

- `render_version` は 3 だけが読める（`validate`。試験 `only_render_version_three_and_format_five_load` が 4 を `Unsupported render version` として固定している）。
- 中間はすべて Q14（共通の現状）。`target_format(exact: bool)` が `Rgba16Uint`（v3）と `Rgba16Float`（v1、v2 の名残）を返し、`Resources::new(device, governor, exact)` が v3 の段を作る。`compose_tiles` は `exact` が変わると資源を作り直す。
- `prepare_scene` は v3 で 17 以上の合成モードを `INVALID_ARGUMENT`、`v3_gap` を `GPU_UNSUPPORTED` にし、表示タイルの形式（`Rgba8Uint` か `Rgba8Unorm`）を切り替える。
- `v3_shader_sources_are_pinned` が `integer_composite_shader` と `integer_effects_shader` のハッシュを固定する。v3 の WGSL のファイルを変えるとこの試験が落ちる。
- 製品の CPU 経路の v3 の式：`raster_q8::vector_pixels_q14`（`Rasterize`）、`raster_q8::color_q14`。
- メモリ：`cache_layers` と `gpu_bytes` が 1画素 4 バイトを前提にする。`Governor` は確保ごとに `create_texture` の形式から大きさを数えるので、中間の形式を変えれば計上値も自動で変わる。

### 11-04 からの変化

- 経路の名前が変わった（`4121d69`、`725d6f4`、`03d7081`）。11-04 版の `renderer/m5.rs`、`cpu_composite.rs`、`composite_q30.rs` を今の名前に直した。
- 11-04 版の「`Wide` は `effects-v3-wide.wgsl`」は誤り（冒頭）。
- `renderer/memory.rs` は `7bdbe92` で計上値の合計を差分で保つ形になった。11-04 版の「`Governor` が見積もる一時ターゲットの量を形式ごとに分ける」は、`Governor` が見積もらず実際の確保を数えるので不要。分けるのは `cache_layers` と `live_fixed` の1画素のバイト数だけ。
- 製品の CPU 経路（`Rasterize`）にも v3 の式があることを、11-04 版は書いていなかった。
- 11-04 版の「v4 は文字のヒンティングを使う文書で使う」は、06-text の TXT-10 が `DocumentMeta.text_rendering`（新規）として設計した。

### 設計

`DocumentMeta::render_version` を 3 または 4 にする。v3 は 8bit、ガンマ合成の定義として残し、既存のハッシュを変えない。v4 は合成の中間を Q30 にした新しい定義で、次のどれかを使う文書は v4 でなければならない：`depth` が `U16` か `F16`（COL-2、COL-3）、`blend_space`（新規）が `linear`（COL-4）、`text_rendering` がある（TXT-10）。8bit・ガンマのままの v4 も、明示すれば作れる（決定が要る点）。版は作成時に決まり、途中で変えない（COL-1 の不変の検査）。

**定数と型。**

- `alto_core::RENDER_VERSION_Q30: u32 = 4`（新規）。
- `alto_core::RenderPath { Q14, Q30 }`（新規）と `RenderPath::of(render_version: u32) -> Option<RenderPath>`（新規）。`render_version` を直接比べている箇所（共通の現状の一覧。`WorldTransform::root` とストロークの `fixed_world` を含む）を、すべてこれに置き換える。`WorldTransform::root(path)` は Q30 でも `V3`（整数の変換）を返し、ストロークの変換も Q30 で `FixedAffine` を使う。`>= RENDER_VERSION_EXACT` のままだと v4 が v3 の経路に入るので、置き換えを最初の PR にする。
- `DocumentMeta::validate`：`render_version` は 3 か 4。3 は U8 だけ（`INVALID_ARGUMENT: render version 3 is 8-bit only`）。

**Q30 の定義。** 1.0 = 2^30。値は `i32`、乗算済み。

| 量 | 式 |
|---|---|
| バイト → Q30 | `(b × 2^30 + 127) / 255`（`u64`、切り捨て） |
| u16 → Q30 | `(v × 2^30 + 32767) / 65535`（`u64`） |
| 積 | `round_half_away(a × b / 2^30)`。`a × b` は 2^62 未満で `i64`（WGSL は `Wide`） |
| 非乗算（`c / a`） | `c × 2^30 / a`。丸めは v3 の同じ箇所の規則に従う（下記）。64bit ÷ 32bit（WGSL は `wide_div_u32`（新規）。32 回の引き戻し除算） |
| Q30 → バイト | `min((q × 255 + 2^29) >> 30, 255)`（`q` は 0〜2^30 に切ってから） |
| Q30 → u16 | `min((q × 65535 + 2^29) >> 30, 65535)` |

- 丸めは式ごとに v3 の同じ箇所と同じにする。v3 は箇所ごとに違う：`div_q` と `wide_div_q14` は 0 から遠い方へ四捨五入し、色調整の非乗算（`effects-q14.wgsl` の `color_adjust_q14`、`(v << 14) / qa`）は切り捨てる。上の表は丸めの無い形の式で、各段の Q30 版はその段の v3 の式の丸め（切り捨てか四捨五入か）をそのまま写す。v3 の各式を、Q14 を Q30 に置き換えた形で定義する。v3 の式の Q 値を含む中間（たとえば不透明度とマスクの積の Q28）は、Q30 では `i64` で同じ順序に丸める。
- 位置と標本化（Q8 の位置、Q16 の逆行列、整数の 4 タップ）は色と無関係なので v3 と同じ。`effects-v3-wide.wgsl` の Q8 の補助をそのまま含める。
- ホストで作る表（トーンカーブ、レベル補正のガンマなど `color_adjust` の表）は Q30 版を作る。値は v3 と同じ f32 のビットから整数で求める（`q14_round` と同じ方法で Q30 へ）。

**シェーダー（すべて新規のファイル。v3 のファイルは変えない）。**

| ファイル | 中身 |
|---|---|
| `crates/alto-gpu/shaders/q30.wgsl`（新規）| Q30 の補助、`wide_div_u32`、バイトと u16 の変換。`Wide` の基本演算は `q14.wgsl` と同じ式を写す（`q14.wgsl` を含めると v3 の固定と無関係に二重定義になるため） |
| `blend-q30.wgsl`（新規）| 17 の合成モード（Normal と v3 で定義済みのモード）を Q30 で |
| `effects-q30.wgsl`（新規）| v3 と同じモード番号（8、17〜33）の Q30 版。番号の空間を v3 と共有し、新しい番号は使わない |
| `effects-resolve-q30.wgsl`（新規）| Q30 → 表示タイル（8bit は `rgba8uint`、16bit は `rgba16uint`。COL-2） |
| `glyph-q30.wgsl`（新規）| ベクターと文字の被覆を Q30 へ塗る（`glyph-q14.wgsl` と同じ式） |

- 組み立て：`integer_effects_shader_v4()`（新規。`q30.wgsl`、`blend-q30.wgsl`、`effects-v3-wide.wgsl`、`effects-q30.wgsl`）。試験 `v4_shader_sources_are_pinned`（新規）でハッシュを固定する。
- 中間の形式：`rgba32sint`。`target_format(path: RenderPath)`（引数を変える）が Q14 は `Rgba16Uint`、Q30 は `Rgba32Sint` を返す。1画素 16 バイトで v3 の2倍。
- 経路：段1では v4 の全シーンを `tile_renderer` の段（M5 と同じ組）で描き、平易な合成器（`integer_composite_shader` の v4 版）は作らない。今の選択は `plain_scene` が機能だけで決めるので、そのままだと平易な v4 の文書が v3 でない古い経路（`Rgba8Unorm` の表示タイルと f32 の標本化）に入り、黙って違う画素になる。`plain_scene(scene)` を `plain_scene(scene, path)`（引数を足す）にし、`RenderPath::Q30` では常に false を返す。`prepare_scene` の `integer`（表示タイルを `Rgba8Uint` にするか）と `compose_from_children`（ピラミッドの整数の縮小）の条件は「`RenderPath`（新規）が Q14 か Q30」にする。判定条件：Normal の測定（下記）で、v4 の平易な文書の B1 または B2 が v3 の同じ文書の2倍を超えたら、平易な合成器の v4 版（`composite-q30.wgsl`）を作る。
- `prepare_scene`：v4 の文書で、まだ Q30 の定義が無い機能を `GPU_UNSUPPORTED`（`v4 {gap} has no integer definition yet`）にする。`v4_gap`（新規）を `v3_gap` の隣に置き、移行の段ごとに項目を外す。
- 一様な部分木の最適化（`uniform_subtree`）は v4 では無効のまま始める。

**3実装。**

- Rust の参照：`crates/alto-gpu/src/cpu_composite/q30.rs`（新規）、`q30_stages.rs`（新規）。`i64` と `checked_*`。和が溢れうる段（ぼかしの重み付きの和）は `i128`。
- JS の参照：`tests/e2e/cpu-composite.js` に v4 の分岐（`renderCase(case, 4)`）。2^53 を超える積は BigInt。Rust と JS の突き合わせは `tests/e2e/m5-v4-renderer-reference.mjs`（新規）を `reference_tests.rs` から呼ぶ。
- WGSL：上の表。
- 固定：`tests/parity/cpu-reference-v4.txt`（新規）は Rust の参照の blake3 と FNV、`tests/parity/v4-browser.txt` はブラウザーの readTile。Metal、lavapipe、SwiftShader で全バイト一致。既存の `tests/parity/*.txt` の行は変えない。

**製品の CPU 経路。** v4 の `Rasterize` は `raster_q8::vector_pixels_q30`（新規）、色は `raster_q8::color_q30`（新規）。`MergeDown` の `composite_into` は今の独自の Normal のまま（`render_version` に依らない式なので、8bit・ガンマの v4 でも同じバイト）。16bit とリニアの対応は COL-2、COL-4 が行う。

**ブラシ（render v4 での扱い）。** ブラシは v4 でも GPU の f32 スタンプと読み戻しが正本で、共通規則2（3実装の全バイト一致）の外（03-brush）。v4 は合成と表示だけを変え、`brush.wgsl` と `cpu_paint` は 8bit の文書では v3 と同じ。ストロークの確定は従来どおり読み戻したタイルを `Op::SetTiles` に入れる。16bit の文書のブラシは COL-2 が `brush.wgsl` の出力形式を変える（同じ f32 の式で、量子化だけが変わる）。表示中のストロークは v4 の合成器が描く。

**テキスト（render v4 での扱い）。** 焼き込み（ALTB、`alto_core::text_bake`、`BAKER_VERSION`）と `glyph_q8::glyph_bitmaps` の被覆は色と精度に依らないので、v4 でも同じバイトを使う。v4 は被覆を `glyph-q30.wgsl` で Q30 の中間へ塗るだけ。`tests/parity/text-bake.txt` は変わらない。TXT-10（06-text）は v4 の文書だけに `text_rendering`（新規）（ヒンティング、ステム強調、被覆のガンマ）を足し、被覆を作る段（`glyph_q8`）で効く。v4 の文書でも `text_rendering` が無ければ被覆は v3 と同じバイト。

**移行の順序。** 段ごとに PR を分け、段ごとに parity の行を足し、3バックエンドで全バイト一致させてから次へ進む。

1. `RenderPath`（新規）への置き換え（v3 の出力不変。v4 の文書はまだ作れない）。
2. Normal の合成（ラスターだけ、不透明度、可視）。ここで測定する。
3. 17 の合成モード、分離グループ。
4. マスク（ラスター、ベクター、クリップ）。
5. 効果（ぼかし、色調整、レベル補正、トーンカーブ、色相・彩度、反転）と調整レイヤー。
6. ベクター（`glyph-q30.wgsl`、`vector_pixels_q30`（新規））。
7. テキスト。
8. `createDocument({ renderVersion: 4 })` を SDK に公開する（`v4_gap`（新規）が空になってから）。

段1〜7の試験と測定には v4 の文書が要るので、段1で試験用の入口を作る。`DocumentMeta::validate` は段1から 4 を受ける。WASM は `Runtime::create_document_for_testing`（新規）（js_name `createDocumentForTesting`）だけが `render_version: 4` を受け、公開の `create_document` は段8まで `INVALID_ARGUMENT: render version must be 3` のまま。SDK は `testing(engine).createDocument({ ..., renderVersion: 4 })`（新規。`packages/alto-sdk/src/testing.ts`）からだけ呼ぶ。未定義の機能は `v4_gap` が `GPU_UNSUPPORTED` にするので、試験用の文書でも黙って違う画素にはならない。`.alto` を開く経路は段1から 4 を受ける（`validate` を通るため）。段8で公開の入口へ移し、試験用の入口は残す。

**測定と判定条件（ROADMAP「測定で実装の形が変わる項目」）。** 段2の試作で、4K・20層（`tests/e2e/browser-tests.js` の B1〜B6 と同じ文書）を Metal で3回測る。

- 測るもの：B1、B2、B6。B6 は GPU の計上値（`gpuBytes` と `operationPeak`）、WASM と復号プールの和、メインスレッドの JS heap を分けて記録する。GPU の計上値を VRAM と呼ばない。
- 予算：`memoryBudgetMB` を 2048 にして、`Governor` が上限で切らない状態の最大を測る。既定の 512 でも同じ文書を描き、`RenderError::Budget` が出るかを記録する。
- 16bit の見積り：段2の試作では、アトラスと表示タイルを `rgba16uint` にした場合（8bit のタイルを 257 倍して入れる）も測る。COL-2 の判定はこの値で行う。
- 判定：16bit の場合の GPU の計上値の最大、または WASM と復号プールの和が 1 GiB を超えるなら、16bit と F16 の文書は辺長 8192 までに制限する（`MAX_DEEP_EDGE = 8192`（新規）、COL-2）。
- 記録：`doc/verification/RV4-Q30-MEASURE.md`（新規）。結果で ROADMAP の該当行を更新する。

**GPU 喪失。** v4 の資源は `Resources::new(device, governor, path)`（引数を変える）で作る。喪失後の作り直しは今と同じ経路で、文書の `render_version` から `RenderPath`（新規）を決める。

**上限とエラー。** 新しい上限は無い（予算は既存の `Governor`）。v4 で未定義の機能は `GPU_UNSUPPORTED`、未対応の合成モードは v3 と同じ `INVALID_ARGUMENT`。

### 形式と通信

- `format_version`：上げる（`render_version` 4 を読めるようにする。13）。「共通の設計：形式番号を上げる経路」の基盤は回 6 の PR が先に入れるので、この PR は `SUPPORTED_FORMATS` に 13 を足すだけ。06-text の TXT-10（`DocumentMeta.text_rendering`（新規））、COL-2、COL-4（`blend_space`）と同じ回 13。COL-1 を「意味を増やす変更」とみなす場合も 13。
- `PROTOCOL`：上げる（`createDocument` の `renderVersion: 4`。12。段8でだけ上げ、COL-2 以降の v4 の項目と同じ回）。
- render v3 の画素：変えない。`v3_shader_sources_are_pinned` と既存の parity が通ること。
- シェーダーのモード番号：新しい番号は要らない。v4 は v3 と同じ番号の空間を使い、各番号の Q30 版を持つ。02 が v3 に足すモード（34〜43。07 はモードを使わない）は、v4 にも同じ番号で Q30 版を足す。
- manifest：`document_format` を文書の `format_version` から書く（共通の設計）。manifest の `format` は変えない。

### 作業手順

**Files:**
- Modify `crates/alto-store/src/lib.rs`（manifest の `document_format`）、`crates/alto-engine/src/versions/gc.rs`、`crates/alto-engine/src/versions/merge.rs`（共通の設計の形式番号の経路。経路は回 6 の PR が入れたものを使い、`SUPPORTED_FORMATS` に 13 を足す）。
- Modify `crates/alto-core/src/lib.rs`（`RENDER_VERSION_Q30`、`RenderPath`）、`crates/alto-doc/src/lib.rs`、`crates/alto-doc/src/document.rs`（試験）。
- Modify `crates/alto-engine/src/lib.rs`、`crates/alto-engine/src/text.rs`、`crates/alto-scene/src/damage.rs`（`plain_scene` を含む）、`crates/alto-gpu/src/sample.rs`、`crates/alto-gpu/src/renderer/geometry.rs`（`WorldTransform::root`）、`crates/alto-wasm/src/lib.rs`（ストロークの `fixed_world`、`create_document_for_testing`）、`packages/alto-sdk/src/testing.ts`（`RenderPath` への置き換えと試験用の入口。段1）。
- Modify `crates/alto-gpu/src/lib.rs`（`integer_effects_shader_v4`）、`crates/alto-gpu/src/renderer.rs`（`shader_tests`）、`crates/alto-gpu/src/renderer/scene.rs`、`renderer/compose.rs`、`renderer/uniform.rs`、`renderer/geometry.rs`（`v4_gap`）、`renderer/tile_renderer/mod.rs`、`tile_renderer/stages.rs`、`tile_renderer/paint.rs`、`tile_renderer/targets.rs`、`tile_renderer/params.rs`、`crates/alto-gpu/src/raster_q8.rs`。
- Create `crates/alto-gpu/shaders/q30.wgsl`、`blend-q30.wgsl`、`effects-q30.wgsl`、`effects-resolve-q30.wgsl`、`glyph-q30.wgsl`（いずれも新規）、`crates/alto-gpu/src/cpu_composite/q30.rs`、`q30_stages.rs`（新規）、`tests/e2e/m5-v4-renderer-reference.mjs`（新規）、`tests/parity/cpu-reference-v4.txt`、`tests/parity/v4-browser.txt`（新規）、`doc/verification/RV4-Q30-MEASURE.md`（新規）。
- Modify `crates/alto-gpu/src/cpu_composite/mod.rs`、`cases.rs`、`reference_tests.rs`、`gpu_parity_tests.rs`、`tests/e2e/cpu-composite.js`、`tests/e2e/parity-tests.js`、`tests/e2e/browser-tests.js`（測定）。
- 段8：Modify `crates/alto-wasm/src/lib.rs`（`CreateOptions.render_version`）、`packages/alto-sdk/src/engine.ts`、`protocol.ts`、`types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、README、RELEASING。

**インターフェース：** Produces `RENDER_VERSION_Q30`、`RenderPath`、`integer_effects_shader_v4`、`target_format(path)`、`v4_gap`、`vector_pixels_q30`。Consumes COL-1 の `CreateOptions`、`DocumentMeta::new`、不変の検査。

**試験：**
- [ ] 段1：`v3_documents_keep_every_hash`（`git diff tests/parity` が空、`v3_shader_sources_are_pinned` が通る）。振る舞いの試験：`v3_scene_selects_q14_resources` と `v4_scene_selects_q30_resources`（v3 と v4 の同じシーンを描き、`tile_renderer` の資源の `RenderPath` と中間の形式（`Rgba16Uint` / `Rgba32Sint`）、表示タイルの形式、`uniform_subtree` が v4 で使われないこと、文字が焼き込みの経路に入ることを確かめる）。`damage_treats_v3_and_v4_by_path`（`alto-scene` の損傷の計算）。`plain_v4_scene_is_not_plain`（ラスターだけの v4 の文書が `compose_tiles` に入り、`Rgba8Unorm` の表示タイルを作らない）。`v4_world_transform_is_fixed`（`WorldTransform::root` とストロークの変換が v4 で整数の変換になる）。補助として、`render_version >=` と `render_version ==` が `RenderPath::of` の中にしか無いことを `git grep` で確かめる（これだけでは振る舞いを保証しない）。
- [ ] `crates/alto-doc`：`render_version_four_validates_and_three_is_eight_bit_only`。`only_render_version_three_and_format_five_load` を、4 を受ける形に直す。
- [ ] `q30_conversions_roundtrip_bytes_and_u16`：全バイトと全 u16 が Q30 を経て元に戻る。
- [ ] `wide_div_u32_matches_u64_division`：境界値（0、1、2^32−1、2^62 付近）と乱数でない固定の列。
- [ ] 段2：`q30_normal_matches_the_cpu_reference`（Rust、JS、Metal、lavapipe、SwiftShader）。`rust_and_js_v4_references_agree`。
- [ ] 段3〜7：段ごとの parity の行（`v4-blend-*`、`v4-mask-*`、`v4-effect-*`、`v4-vector-*`、`v4-text-*`）。段ごとに `v4_gap` の項目が1つ減り、残りが `GPU_UNSUPPORTED` のまま。
- [ ] `v4_text_uses_the_v3_coverage_bytes`：同じ焼き込みで `glyph_bitmaps` の出力が v3 と同じ。
- [ ] `render_version_cannot_change_in_a_transaction`（COL-1 の検査を v4 で）、`merge_refuses_a_v3_and_v4_pair`。
- [ ] `v4_document_survives_save_reopen_commit_diff_merge`。
- [ ] `gpu_loss_rebuilds_the_v4_resources`（`testing(engine).destroyGpu()` の後、全タイルが同じバイト）。
- [ ] `v4_budget_overrun_is_an_explicit_error`（小さい `memoryBudgetMB` で `INVALID_ARGUMENT` の予算エラー、文書は変わらない）。
- [ ] 測定：`doc/verification/RV4-Q30-MEASURE.md`（新規）に3回分の B1、B2、B6（予算 2048 と 512、8bit と 16bit の見積り）と判定を記録する。

**完了条件：** 段8まで終わり、v3 の全機能に v4 の定義があり、v4 の3実装（Rust、JS、WGSL）が Metal、lavapipe、SwiftShader で互いに全バイト一致する。v4 の画素は v3 と一致することを求めない（Q14 と Q30 で丸めの位置が違い、同じ入力でも最終のバイトが違いうる。違いは v4 の parity の行として固定する）。v3 のハッシュが1つも変わらない。測定の判定を記録した。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`node scripts/verify-browser.mjs --suite=parity`（Metal、`ALTO_BROWSER_GPU=swiftshader`、lavapipe）、`node scripts/verify-browser.mjs --benchmark-only`（測定）。

### 依存

- 先に要る項目：COL-1（`CreateOptions`（新規）、`DocumentMeta::new`、不変の検査）。ROADMAP の順序では COL-5 の後。形式番号の基盤（`FORMAT_VERSION`、`SUPPORTED_FORMATS`（新規））は回 6 の PR（06）が先に入れる。PERF-4（10）を実装するなら RV4-1 の前に入れ、v4 の段は PERF-4 の明示タスクの形で足す（00-ROADMAP の重なりの表）。
- 後に続く項目：COL-2、COL-4、COL-3、TXT-10（06）。
- 影響を受ける項目：RV4-1 より前に v3 へ入る描画の項目（02 の FX、07 の VEC-1 など）は、段5〜7で Q30 版も作る。RV4-1 より後に入る描画の項目は、v3 と v4 の両方を作る（ROADMAP「描画経路の二重化」）。

## COL-2 16bit

### 現状

- `PixelFormat` は `Rgba8` と `R8` だけ。タイルのハッシュの tag、オブジェクトの tag と大きさ、復号 Worker の形式のバイトはこの2つを前提にする（共通の現状）。`crates/alto-engine/src/lib.rs` のタイルの読み込み（オブジェクトの tag から形式を決める箇所）と `crates/alto-wasm/src/lib.rs`（形式から tag を作る箇所）も同じ。
- 内容のアトラスは `Rgba8Unorm`、表示タイルは `Rgba8Uint`、`readTiles` は1タイル 262144 バイト。
- ブラシの出力は `rgba8unorm`（`brush.wgsl`）。
- 画像編集（`ImageEdit` の `Crop`、`Resize`、`Rotate`、`Flip`、`Gradient`、`Fill`、`CloneStamp`、`Smudge`）、`CanvasEdit`、`LayerEdit`（`MergeDown`、`Rasterize`）、`image_histogram`（256 区分）、`pick_color` は RGBA8 だけ。
- 読み込みはブラウザーの 2D canvas を通るので 8bit になる。16bit の PNG を損失なく読む経路は無い。
- 書き出しの PNG は 8bit（`encodePng`）。

### 11-04 からの変化

- 旧 crates/alto-engine/src/image.rs は `crates/alto-engine/src/image/`（`mod.rs`、`pixels.rs`、`resample.rs`、`stroke.rs`、`selection.rs`）に分かれた（`126f2b5`）。
- `crates/alto-store/src/lib.rs` は版を `.alto` の外に置く形（manifest format 3、`8c6d2fd`）などで変わった。オブジェクトの tag の扱いは変化なし。外の置き場にも同じ tag のオブジェクトが入る。
- 書き出しが表示タイルからの PNG 符号化になった（`0634560`）。11-04 版の「`exportImage` の 16bit PNG」は、表示タイルが 8bit なのでそのままでは作れない。表示タイルを 16bit にする形にした。
- 11-04 版は「ブラシの読み戻しは `rgba16unorm`」としたが、`rgba16unorm` は WebGPU の基本の形式に無い（拡張の機能が要る）。`rgba16uint` に直した。

### 設計

`depth: U16` の文書は、ラスターの画素を 16bit の `PixelFormat::Rgba16`（新規）で持つ。render v4 が必須。

**タイル。**

- `PixelFormat::Rgba16`（新規）：直線アルファ、各チャネル `u16` の little-endian。1タイル 256×256×8 = 512 KiB。`bytes_per_pixel` は 8。
- ハッシュの tag は 8（`ContentHash::of_tagged(8, …)`）。オブジェクトの tag も 8、大きさ 524288。`object_header` が受ける tag に 8 を足す。不透明の判定（`alto-store` の `opaque`）は alpha が 65535。
- 復号 Worker の形式のバイトを明示する：0 = RGBA8、1 = R8、2 = RGBA16（新規）。不明な値は壊れたデータ。
- 1つの文書のラスターは、文書の深さの形式だけ（U16 の文書の RGBA8 タイル、U8 の文書の RGBA16 タイルは `CORRUPT_DATA`）。マスクと選択は R8 のまま。

**描画。**

- アトラスと表示タイルは `rgba16uint`（値は u16、表示タイルは乗算済み）。`effects-resolve-q30.wgsl` が Q30 → u16 に戻す。
- `cache_layers(budget_mb, max_layers, tile_bytes)`（引数を足す）と `gpu_bytes` の `live_fixed` が、文書の形式の1タイルのバイト数を使う。同じ予算で層数が半分になる。
- ピラミッドは `downsample-u16.wgsl`（新規。`downsample-q14.wgsl` と同じ平均を u16 で）。提示は `present-u16.wgsl`（新規、f32、表示だけ）。
- `readTiles` は形式に応じて 1タイル 524288 バイトを返す。`readback.rs` の容量はバイトで数える。

**ブラシ。** 共通規則2の外のまま（RV4-1）。16bit の文書では `brush.wgsl` の `base` を `texture_2d<u32>`（`rgba16uint`）、`output` を `texture_storage_2d<rgba16uint, write>` にする。f32 の式は同じで、読み込みは `/65535`、書き込みは `round(clamp(x, 0, 1) × 65535)`。8bit の文書のシェーダーは変えない（別の入口 `brush16`（新規）か、定数で分ける）。`cpu_paint` も u16 版を持つ。

**画像編集と CPU の経路。** 画素の型を引数にする。

- `trait Channel`（新規）を `u8` と `u16` に実装し、`image/pixels.rs`、`resample.rs`、`stroke.rs`、`selection.rs`、`canvas.rs` の画素の処理をこの型で書く。値の正規化は `Channel::MAX`。
- グラデーションと塗りの色は、16bit では `ImageColor` を `round(c × 65535)` にする。
- `MergeDown` の `composite_into` は u16 版。`Rasterize` は `vector_pixels_q30`（新規）の結果を u16 へ。
- `image_histogram` は 256 区分のまま（上位 8bit）。`pick_color` は `bytes` に加えて `wide: [number, number, number, number]`（新規、u16）を返す。

**読み込み。** ブラウザーの 2D canvas は 8bit なので、16bit の PNG は Rust で復号する。

- 復号器は VEC-3（07）が足す `png` crate を使う（00-ROADMAP の重なりの表）。16bit の PNG は `Engine` で直接タイルにし、ICC の変換は COL-5 の `apply_rgba16`（新規）で行う。
- 8bit の画像を 16bit の文書へ読み込むときは `v × 257`。
- 上限：16bit の文書の読み込みは 32M 画素（決定が要る点）。超えたら `RESOURCE_LIMIT`。

**書き出し。** `exportImage({ depth?: 8 | 16 })`（新規の選択肢、既定は文書の深さ）。16bit の表示タイル（乗算済み u16）は Worker で非乗算に戻す：`a = 0` は (0, 0, 0, 0)、それ以外は `round(c × 65535 / a)`（0.5 は切り上げ、整数で `(c × 65535 × 2 + a) / (2a)`）、`a` はそのまま。結果は `Uint16Array`。16bit の PNG は `encodePng` に `bitDepth: 16`（big-endian のサンプル）を足す。8bit の書き出しは、非乗算の u16 を `(v × 255 + 32767) / 65535` で落とす。`pickComposite` も同じ規則にする。`doc/design/EXPORT-CONTRACT.md` の「出力の画素」に 16bit の式と保証を足す。拡大縮小と WebP は 8bit に落としてから canvas。Worker の作業量の見積りは1画素のバイト数を形式から取る（上限 64M 画素、512 MiB は変えない）。

**メモリ。** 同じ画素数で、WASM のタイル、復号キャッシュに入るタイルの数、`.alto` の大きさ、GPU の計上値（アトラス、表示タイル）が倍になる。`.alto` の 512 MiB の上限には半分の画素数で届く（F-9 の版の外置きで過去の版は外へ出せる）。RV4-1 の判定が「1 GiB を超える」なら `MAX_DEEP_EDGE = 8192`（新規）を `validate` と `CanvasEdit::Resize` に入れ、超えたら `INVALID_ARGUMENT`（今の辺 65536 と同じコード）。

**SDK。** `createDocument({ depth?: "u8" | "u16" })`（新規。`"u16"` は `renderVersion: 4` を要する。省略時は 4 にする）。`DocumentMeta.depth` を `"u8" | "u16" | "f16"` に狭める。`Transaction` のラスターの作成（`transaction.ts` の `tiles: { format: "rgba8" }`）は文書の深さに合わせる。

### 形式と通信

- `format_version`：上げる（`PixelFormat::Rgba16`（新規）、tag 8 のオブジェクト。13。RV4-1 と同じ回）。
- `PROTOCOL`：上げる（`createDocument` の `depth`、`exportImage` の `depth`、`readTiles` の大きさ。12。RV4-1 段8と同じ回）。
- render：v4 のみ。v3 の画素は変えない。
- manifest：`document_format` は共通の設計の経路で新しい番号を書く。オブジェクトの tag は manifest の外。

### 作業手順

**Files:**
- Modify `crates/alto-core/src/lib.rs`（`PixelFormat`）、`crates/alto-raster/src/lib.rs`、`crates/alto-raster/src/brush.rs`（`cpu_paint` の u16 版）、`crates/alto-store/src/lib.rs`、`crates/alto-decode/src/lib.rs`、`crates/alto-doc/src/lib.rs`、`crates/alto-doc/src/node.rs`。
- Modify `crates/alto-engine/src/lib.rs`、`crates/alto-engine/src/canvas.rs`、`crates/alto-engine/src/image/mod.rs`、`pixels.rs`、`resample.rs`、`stroke.rs`、`selection.rs`、`crates/alto-engine/Cargo.toml`（`png`）。
- Modify `crates/alto-gpu/shaders/brush.wgsl` か Create `crates/alto-gpu/shaders/brush16.wgsl`（新規）、Create `downsample-u16.wgsl`、`present-u16.wgsl`（新規）。Modify `crates/alto-gpu/src/renderer/memory.rs`、`renderer/device.rs`、`renderer/gpu.rs`、`renderer/scene.rs`、`renderer/stroke.rs`、`renderer/readback.rs`、`renderer/pyramid.rs`、`renderer/frame.rs`。
- Modify `doc/design/EXPORT-CONTRACT.md`（16bit の表示タイルと PNG）。
- Modify `crates/alto-wasm/src/lib.rs`、`crates/alto-wasm/src/image_versions.rs`、`packages/alto-sdk/src/engine.ts`、`worker.ts`、`document.ts`、`png.ts`、`transaction.ts`、`types.ts`、`image-types.ts`、`protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`packages/alto-sdk/src/decode-worker.ts`。
- Modify `fuzz/fuzz_targets/package.rs`、`snapshot.rs`（tag 8 を種に足す）。

**試験：**
- [ ] `rgba16_tile_roundtrips_and_hashes_with_tag_8`、`object_header_accepts_tag_8_of_524288_bytes_only`、`decoder_format_byte_2_is_rgba16`。
- [ ] `mixing_8_and_16_bit_tiles_in_a_document_is_refused`。
- [ ] `image_edits_keep_16_bits`：`Resize`（nearest、area）、`Gradient`、`Fill`、`MergeDown`、`Crop`、`Rotate`、`Flip` で、下位バイトが 0 でない入力の下位バイトが保たれる。
- [ ] `eight_bit_documents_are_unchanged`（画像編集の既存の試験と parity がそのまま通る）。
- [ ] `u16_brush_cpu_paint_matches_the_u8_shape`：同じダブで u16 版を 257 で割った値が u8 版の ±1。
- [ ] ブラウザー試験：`a 16 bit stroke commits through readback`（`cpuBrushReference` の u16 版と ±2×257）、`a 16 bit PNG imports and exports without loss`（全 u16 一致）、`an 8 bit image in a 16 bit document scales by 257`。
- [ ] `sixteen_bit_document_survives_save_reopen_commit_diff_merge`、外置きの履歴（`save(target, { history: { folder } })`）でも同じ。
- [ ] `v4-u16-*` の parity（Normal、合成モード、効果）。
- [ ] 判定が「制限」なら `deep_document_over_8192_is_refused`（作成とキャンバスの拡縮）。

**完了条件：** 上の試験が通り、8bit の文書の画素とハッシュが変わらない。RV4-1 の判定に従う上限が入っている。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`node scripts/verify-browser.mjs --suite=parity`、`sh fuzz/run-all.sh`。

### 依存

- 先に要る項目：RV4-1（Q30 と測定の判定）、COL-5（16bit の ICC 変換。無ければ 16bit の読み込みは sRGB とみなす）。
- 後に続く項目：COL-3（`Rgba16` の格納を共有）、COL-4（16bit でのリニア合成）。
- 同じものを使う項目：VEC-3（07。`png` crate）、STO-3（09。`ExportImageOptions` を同時に拡張する）、IMG-1（05。焼き込みの出力形式）、BR-1〜BR-5（03。`brush.wgsl` を同時に変える。BR-1〜BR-3 の binding 6/7 と `GpuDab.extra` が先に入り、本項目はその上に 16bit 版を足す（00-ROADMAP の重なりの表））。

## COL-4 リニア合成

### 現状

- 合成は文書の色空間の符号化値（ガンマ）のまま行う（v3 の全段）。文書に合成の空間の属性は無い。
- 線形化の関数は `image/pixels.rs` の `linear`、`srgb`（f64、グラデーションの補間だけ）で、文書の合成には使わない。
- `MergeDown` は独自の Normal（ガンマ）。

### 11-04 からの変化

- 変化なし（`crates/alto-doc/src/lib.rs` の分割（`126f2b5`）は内容を変えていない）。
- 11-04 版の「TRC は 4096 点の整数表を uniform のストレージに置く」を、8bit と 16bit で表を分け、往復の誤差の判定条件を付けた形に具体化した。

### 設計

文書属性 `DocumentMeta.blend_space: BlendSpace`（新規。`Gamma` | `Linear`、`serde(default)`、既定の `Gamma` は直列化しない）。`Linear` は render v4 を要する。作成時に決まり、不変の検査に加える。

**どこで線形にするか。**

- 画素を読む段（アトラスの取得、ベクターと文字の色、調整の入力）で TRC を外し、線形の Q30 で合成、効果、マスクの適用を行い、解決の段（`effects-resolve-q30.wgsl`）で TRC を戻す。マスクと被覆（アルファ）は線形化しない。
- 乗算済みの値の線形化は、非乗算にしてから TRC を外し、乗算し直す（アルファ 0 は 0）。

**TRC の表（3実装で同じ表）。**

- 8bit → 線形：256 点の Q30 の表（正確な表）。
- 16bit → 線形：4097 点（`v >> 4` で引き、下位 4bit で線形補間、四捨五入）。
- 線形 → 符号化：線形の値を 4096 等分した 4097 点の表と線形補間で u16 相当（Q30）を求め、最後にバイトか u16 へ量子化する。
- 文書の色空間ごと（sRGB と P3 は同じ TRC なので1組）。表は生成物として `crates/alto-gpu/src/trc_tables.rs`（新規）にコミットし、表そのものを定義とする。WGSL へはストレージのバッファで、JS へは同じ表を JSON（`tests/fixtures/trc-tables.json`（新規））で渡す。
- 判定条件：16bit の全 65536 値で `encode(decode(v))` と `v` の差が 1 以下であること。超えるなら点数を 16385 に増やして測り直す。8bit は全 256 値で一致すること。

**ほかの経路。** `MergeDown` は線形で合成する（COL-2 の u16 版と同じ関数に `blend_space`（新規）を渡す）。ブラシのストロークは今のまま符号化値の上で描く（f32 の式を変えない。ブラシの見た目の線形化は範囲外）。グラデーションの補間は今も線形なので変えない。文字は線形合成で細く見えるので、TXT-10 の被覆のガンマと組み合わせる。

**SDK。** `createDocument({ blendSpace?: "gamma" | "linear" })`（新規）。`"linear"` は `renderVersion: 4` を要する（省略時は 4 にする）。

**エラー。** v3 で `blend_space: linear` は `INVALID_ARGUMENT`（`linear blending requires render version 4`）。

### 形式と通信

- `format_version`：上げる（`blend_space`（新規）。13。RV4-1、TXT-10 と同じ回）。
- `PROTOCOL`：上げる（`createDocument` の `blendSpace`（新規）。12）。
- render：v4 のみ。v4 の `Gamma` の文書の画素は変えない。
- シェーダーのモード番号：要らない（取得と解決の段の分岐で、モードではない）。

### 作業手順

**Files:**
- Modify `crates/alto-doc/src/lib.rs`（`BlendSpace`、検証、不変の検査）、`crates/alto-engine/src/canvas.rs`（`MergeDown`）。
- Create `crates/alto-gpu/src/trc_tables.rs`（新規）、`tests/fixtures/trc-tables.json`（新規）。
- Modify `crates/alto-gpu/shaders/effects-q30.wgsl`（新規）、`effects-resolve-q30.wgsl`、`glyph-q30.wgsl`（RV4-1 の新規ファイル）、`crates/alto-gpu/src/renderer/tile_renderer/params.rs`、`crates/alto-gpu/src/cpu_composite/q30.rs`、`q30_stages.rs`、`tests/e2e/cpu-composite.js`。
- Modify `crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/engine.ts`、`protocol.ts`、`types.ts`、`packages/alto-sdk/api/alto-engine.api.md`。

**試験：**
- [ ] `trc_tables_roundtrip_every_byte_exactly`、`trc_tables_roundtrip_every_u16_within_one`（判定条件）。
- [ ] `trc_tables_match_the_json_fixture`。
- [ ] `blend_space_requires_render_v4`、`blend_space_cannot_change`。
- [ ] `gamma_v4_pixels_are_unchanged_by_this_item`（v4 の既存の parity の行が変わらない）。
- [ ] parity の行 `v4-linear-normal`、`v4-linear-multiply`、`v4-linear-blur`、`v4-linear-mask`、`v4-linear-u16-normal`（3バックエンド）。
- [ ] `linear_merge_down_matches_the_linear_composite`。

**完了条件：** 上の試験が通り、判定条件を満たす表がコミットされている。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`node scripts/verify-browser.mjs --suite=parity`。

### 依存

- 先に要る項目：RV4-1。16bit の線形は COL-2 の後。
- 後に続く項目：TXT-10（06。線形合成での文字の見た目）、COL-3（F16 の線形）。

## COL-3 F16 / HDR

### 現状

- `BitDepth::F16` は型だけがあり、`validate` が拒否する。浮動小数の画素の格納、拡張範囲の表示、OpenEXR はどれも無い。
- 表示の面は 8bit の非 sRGB 形式（`renderer/device.rs`）。wgpu 30.0.1 は `Rgba16Float` の canvas に拡張範囲の色空間（wgpu の SurfaceColorSpace::ExtendedSrgb と Display P3 の拡張）を渡せる。

### 11-04 からの変化

- 変化なし（関係するコードは無いまま）。
- 11-04 版の「`toneMapping: { mode: "extended" }`」は、wgpu 30.0.1 の SurfaceColorSpace の拡張範囲の値が同じ設定を canvas に渡すので、wgpu の設定で書く形にした。

### 設計

浮動小数は GPU 間でビット一致せず、「正本は決定的」の前提と両立しない。そこで**正本は固定小数**とする。名前は既存の `BitDepth::F16` のままで、意味を文書化する。

- 格納：`PixelFormat::Rgba16`（新規）（COL-2）に Q3.13（1.0 = 8192、範囲 [0, 8)）で入れる。アルファも Q3.13 で 0〜8192（8192 を超える値は `CORRUPT_DATA`）。直線アルファ。
- 中間：v4 の Q30 の範囲を [−8, 8) に広げた Q27（1.0 = 2^27）。シェーダーは同じファイルで、1.0 の位置を uniform の `one_shift`（新規）（30 か 27）で渡す。Q30 の式は 2^30 を `1 << one_shift` に置き換えて同じ順序で丸める。
- 合成モード：Normal、Multiply、Add は範囲 [0, 8) のまま計算する。ほかのモードは各入力を [0, 1] に丸めてから v4 の式（決定が要る点）。負の結果は正本のタイルで 0 に切る。
- 効果：ぼかしは範囲のまま。色調整、レベル補正、トーンカーブ、色相・彩度は入力を [0, 1] に丸める（同じ決定に従う）。
- 表示：表示タイルは Q3.13 の `rgba16uint`。面を `Rgba16Float` にし、`color_space` を文書の色空間の拡張（sRGB は wgpu の SurfaceColorSpace::ExtendedSrgb、P3 は Display P3 の拡張）にする。`present-u16.wgsl` が Q3.13 を f32 にする。面の能力に拡張範囲が無い環境は 1.0 で切り、`engine.display` に `clipped: true` と `range: "standard"`（新規の欄）を出す。
- 読み込み：OpenEXR と 16bit PNG（1.0 = 65535 を 8192 へ）。EXR の half と float は Rust で Q3.13 に量子化する（四捨五入、範囲外は切る、NaN は 0）。ブラウザーの canvas は通さない。
- 書き出し：16bit PNG（1.0 で切る）と OpenEXR（無圧縮、scanline、RGBA の half）。EXR の符号化器と復号器は `crates/alto-store/src/exr.rs`（新規）に自前で書く。対応は無圧縮と、復号だけ ZIP（`zlib` の展開器は COL-5 と同じ）。それ以外の圧縮は `UNSUPPORTED`。
- 上限：EXR の辺 32768、64M 画素（COL-2 の 16bit と同じ上限の決定に従う）、ヘッダー 64 KiB、チャネル 16。

### 形式と通信

- `format_version`：上げる（`depth: f16` の意味を定める。14）。COL-2 と TXT-10 の後に入るので、回 13 とは分ける。
- `PROTOCOL`：上げる（`createDocument` の `depth: "f16"`、EXR の読み込みと書き出し。12。v4 の項目と同じ回）。
- render：v4 のみ。

### 作業手順

**Files:**
- Modify `crates/alto-doc/src/lib.rs`、`crates/alto-gpu/shaders/q30.wgsl`（新規）、`effects-q30.wgsl`、`blend-q30.wgsl`、`effects-resolve-q30.wgsl`、`present-u16.wgsl`（RV4-1、COL-2 の新規ファイル）、`crates/alto-gpu/src/cpu_composite/q30.rs`、`q30_stages.rs`、`tests/e2e/cpu-composite.js`、`crates/alto-gpu/src/renderer/device.rs`。
- Create `crates/alto-store/src/exr.rs`（新規）、`fuzz/fuzz_targets/exr.rs`（新規）。
- Modify `crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/engine.ts`、`document.ts`、`worker.ts`、`protocol.ts`、`types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`doc/design/EXPORT-CONTRACT.md`（EXR と Q3.13 の書き出し）。

**インターフェース：** `createDocument({ depth: "f16" })`、`exportImage({ type: "image/x-exr" })`、`engine.assets.importImage` が EXR を受ける。Consumes RV4-1、COL-2。

**試験：**
- [ ] `values_above_one_survive_compositing`（Normal、Add）、`negative_results_clamp_at_zero_in_the_canonical_tile`、`alpha_above_one_is_corrupt`。
- [ ] `q27_path_matches_q30_for_values_within_one`（[0, 1] の入力で Q27 と Q30 の結果を Q3.13 に落とすと一致する箇所と、違う箇所の上限を記録）。
- [ ] parity の行 `v4-f16-normal`、`v4-f16-add`、`v4-f16-blur`（3バックエンド）。
- [ ] `exr_header_is_valid_and_pixels_roundtrip`、`exr_nan_becomes_zero`、`exr_unsupported_compression_is_refused`、`exr_over_the_limits_is_refused`。
- [ ] ブラウザー試験：`a display without extended range clips and warns`（試験用の入口で能力を外す）。

**完了条件：** 上の試験が通る。8bit と 16bit の文書の画素が変わらない。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`node scripts/verify-browser.mjs --suite=parity`、`sh fuzz/run-all.sh`。

### 依存

- 先に要る項目：RV4-1、COL-2。
- 後に続く項目：COL-6（拡張範囲の文書のソフトプルーフは範囲外とする）。

## COL-6 CMYK、特色

### 現状

- CMYK、LUT のプロファイル、特色を扱う処理は無い。TIFF の書き出しも無い（09-storage-export の STO-3 が `crates/alto-store/src/tiff.rs`（新規）を新規に作る設計）。
- 表示の提示は `present-q14.wgsl`（f32、ハッシュの外）。`engine.viewport` は `packages/alto-sdk/src/viewport.ts`。
- `LayerProps`（`crates/alto-doc/src/node.rs`、`deny_unknown_fields`）に特色の欄は無い。

### 11-04 からの変化

- 変化なし（関係するコードは無いまま）。
- 11-04 版の「実在のフリーの CMYK プロファイルを確保し同梱」は、再配布の可否が未確認なので決定が要る点に移した。試験は合成したプロファイルで行う。
- 11-04 版の `present-q30.wgsl` は、COL-2 の `present-u16.wgsl` に名前を合わせた。

### 設計

文書の合成モデルは RGB が前提で、CMYK を正本にすると全経路が変わる。そこで**編集は RGB のまま**、次を提供する。CMYK のチャネル単位の編集は提供しない。

**LUT のプロファイル（`alto-color` の拡張）。**

- `ColorModel::Lut { a2b: Option<Lut>, b2a: Option<Lut> }`（新規）。`mft1`（8bit）、`mft2`（16bit）、`mAB `、`mBA `（曲線、行列、CLUT の組）を読む。PCS は `Lab ` と `XYZ `。データ空間 `CMYK` を足す。
- CLUT は入力 3 か 4、格子の点は各次元 2〜33（`MAX_CLUT_POINTS = 33`（新規））。値は u16 に正規化して持つ。
- 補間は整数の四面体補間（3入力）と、4入力は K で線形に分けた2つの四面体補間。重みは Q16、結果は四捨五入。
- PCS の変換（Lab ⇄ XYZ）は整数で行う（立方根は `fixed_pow`（新規））。
- 意図：`Perceptual` は `A2B0` / `B2A0`、`Relative` は `A2B1` / `B2A1`（無ければ `A2B0` / `B2A0`）。黒点補償はしない。

**ソフトプルーフ（表示だけ）。**

- 文書→CMYK→文書の色空間の変換を、33³ の 3D LUT（u16）に焼いて提示の段で適用する。LUT は Rust の整数で作る（決定的）。適用は提示のシェーダー（f32 の三線形補間、表示だけ）。
- `engine.viewport.setProof({ profile: Blob | ArrayBuffer, intent?: "perceptual" | "relative" } | null)`（新規）。文書の状態ではなく、ハッシュ、保存、履歴、`dirty` を変えない。
- シェーダー：`present-q14.wgsl` と `present-u16.wgsl` を変えず、`present-proof.wgsl`（新規）を足す。

**CMYK の書き出し。** `exportImage({ type: "image/tiff", colorSpace: { cmyk: profile, intent? } })`（新規の選択肢）。STO-3 の `crates/alto-store/src/tiff.rs`（新規）に CMYK（8bit、PhotometricInterpretation 5、InkSet 1）とプロファイルの埋め込み（タグ 34675）を足す。透明はプロファイルの白（紙）に合成してから変換する。

**CMYK の読み込み。** JPEG と TIFF の CMYK を読み、埋め込みか指定のプロファイルで文書の RGB へ変換する。元の CMYK の値は保持しない（往復は無損失でない）。結果の `profile` に `converted: true` と、警告 `cmyk: true`（新規の欄）を返す。Adobe の反転 CMYK の JPEG（APP14）は反転を戻してから変換する。

**特色。**

- `LayerProps.spot: Option<Spot>`（新規、`serde(default)`、`None` は直列化しない）。`Spot { name: String, lab: [f32; 3] }`。名前は 1〜64 文字、`lab` は L 0〜100、a と b は −128〜127（範囲外は `INVALID_ARGUMENT`）。ラスターのレイヤーだけ（他の種類は `INVALID_ARGUMENT`）。
- 表示：レイヤーのアルファを版の濃度とし、Lab から文書の色空間へ変換した単色として合成する。Lab からの変換はホストで行い、Q14 / Q30 の色として既存の着色の段に渡す（新しい合成の式は作らない）。v3 では変換した色をホストで `color_q14` に落とす。
- 書き出し：TIFF の追加チャネル（ExtraSamples 0、特色ごとに 8bit、InkNames）。
- 特色どうしの重ね刷りの結果は表示で再現しない。この制限を SDK の型の注釈に書く。

**上限。** 特色 16 個／文書（`MAX_SPOTS = 16`（新規））。プロファイルは COL-5 の上限。ソフトプルーフの LUT は 33³×4×2 バイト。

### 形式と通信

- `format_version`：上げる（`LayerProps.spot`。15）。プロファイルは文書に保存しないので、ソフトプルーフと書き出しは形式に触れない。
- `PROTOCOL`：上げる（`setProof`（新規）、`exportImage` の `colorSpace`、CMYK の読み込み。12。v4 の項目と同じ回）。
- render：v3 と v4 のどちらの文書でも使える（特色の表示は既存の着色の段）。既存の parity の行は変えない。特色を持つ文書の行は新しく足す。

### 作業手順

**Files:**
- Modify `crates/alto-color/src/lib.rs`、`parse.rs`、`transform.rs`（COL-5 の新規ファイル）。Create `crates/alto-color/src/lut.rs`、`lab.rs`（新規）。
- Modify `crates/alto-store/src/tiff.rs`（新規）（STO-3 の新規ファイル）。
- Create `crates/alto-gpu/shaders/present-proof.wgsl`（新規）。Modify `crates/alto-gpu/src/renderer/frame.rs`、`renderer/presentation.rs`。
- Modify `crates/alto-doc/src/node.rs`（`Spot`）、`crates/alto-engine/src/lib.rs`（特色の色をシーンへ）、`crates/alto-scene/src/lib.rs`。
- Modify `crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/viewport.ts`、`document.ts`、`worker.ts`、`engine.ts`、`types.ts`、`protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`。
- Modify `fuzz/fuzz_targets/icc.rs`（LUT の種）。

**インターフェース：** Produces `ColorModel::Lut`、`convert_cmyk8_to_rgb8`（新規）、`convert_rgb8_to_cmyk8`（新規）、`proof_lut`（新規）、`engine.viewport.setProof`、`LayerProps.spot`。

**試験：**
- [ ] `tetrahedral_at_grid_points_returns_the_table_values`、`mft2_and_mab_parse_the_same_lut`（同じ表を2形式で書いたプロファイル）。
- [ ] `four_input_clut_interpolates_k_linearly`、`clut_over_33_points_is_refused`。
- [ ] `lab_xyz_roundtrip_within_one_step`。
- [ ] 合成した CMYK プロファイル（試験のコードで生成）で `cmyk_roundtrip_of_in_gamut_colors_is_within_two`。
- [ ] `proof_does_not_change_any_tile_hash`、ブラウザー試験 `proof toggling keeps the document clean`（`dirty` が false のまま）。
- [ ] `cmyk_tiff_has_the_right_tags_and_embedded_profile`、`spot_channels_are_written_per_layer`。
- [ ] `spot_lab_out_of_range_is_refused`、`more_than_16_spots_are_refused`、`spot_on_a_vector_layer_is_refused`。
- [ ] parity の行 `v3-spot-normal`（新しい行。既存の行は変えない）。
- [ ] `adobe_inverted_cmyk_jpeg_imports_correctly`。

**完了条件：** 上の試験が通る。プロファイルの同梱は決定が要る点に従う。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`sh fuzz/run-all.sh`。

### 依存

- 先に要る項目：COL-5（`alto-color`）、STO-3（09。TIFF の符号化器）。v4 の文書での特色は RV4-1 の後。STO-3 の `tiff.rs` は「Rust はヘッダーだけ、画素は JS」の形なので、CMYK の変換を `alto-color`（Rust）で行うと画素が WASM を通る。最初の手順で、64M 画素の CMYK の書き出しを帯ごとに流したときの WASM メモリと作業量（512 MiB）を見積もり直す（00-ROADMAP の重なりの表）。
- 後に続く項目：なし。

## Review Focus

- 非対応画面の P3 文書で、表示が黙って色ずれしないこと。`engine.display.clipped` で知らせる。
- P3 の文書へ sRGB の画像を読み込んだとき、バイトをそのまま解釈しないこと。
- プロファイル付きの画像の読み込みで、変換の有無と理由を結果に残すこと。
- 色の入力が文書の色空間であることが、SDK の型の注釈から読めること（今の「Linear RGBA」は誤り）。
- 異なる色空間・深さ・描画版の文書の間のマージは `UNRELATED_HISTORY` として拒否されること。`SetMeta` で色のモデルを変えられないこと。
- `render_version` の比較を `RenderPath`（新規）へ置き換え忘れて、v4 の文書が v3 の経路に入らないこと。
- v3 のシェーダーのファイルを1バイトも変えないこと（`v3_shader_sources_are_pinned`）。
- メモリの記述で GPU の計上値を VRAM と呼ばないこと。WASM、JS heap、GPU の計上値を分けて記録すること。
