# 色空間とビット深度

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** sRGB / 8bit 固定を解き、広色域と高精度を段階的に扱う。

**Architecture:** `DocumentMeta` の `color` と `depth` は既に型があり、`validate` が sRGB / U8 以外を拒否している。文書の意味を変えずに表示だけ変える項目を先に行い、画素の格納を変える項目を後に回す。

## 現状

- `crates/alto-core/src/lib.rs`：`ColorSpace { Srgb, DisplayP3 }`、`BitDepth { U8, U16, F16 }`。
- `crates/alto-doc/src/lib.rs`：`DocumentMeta::validate` が sRGB / U8 以外を `Invalid` にする。
- タイルは RGBA8 と R8。中間は Q14（`rgba16uint`）で整数演算。
- 合成は文書の色空間のガンマ値のまま行う（設計書 3.3）。
- `crates/alto-engine/src/image.rs` の `linear`、`srgb` はグラデーション用で、文書の合成とは無関係。

## COL-1 Display P3

**設計。** 合成の整数演算は色空間に依存しない。P3 の文書は同じバイト列を P3 のガンマ値として解釈する。変わるのは表示と書き出しの解釈だけで、render v3 のハッシュは不変。

- `DocumentMeta::validate`：`color == DisplayP3 && depth == U8` を許可する。
- 表示：`OffscreenCanvas.getContext("webgpu")` の `configure({ colorSpace: "display-p3" })` を使う。wgpu 経由で渡せない場合は `present-q14.wgsl` で P3→sRGB の行列変換を行い、非対応画面でクリップされることを `engine.status` の警告で知らせる。
- 読み込み：PNG / JPEG のデコードは `createImageBitmap` の `colorSpaceConversion` と `ImageData.colorSpace` で文書の色空間へ変換する。
- 書き出し：`convertToBlob` で `display-p3` の canvas から出す。
- 色の入力：`Color` は文書の色空間の値とする。sRGB からの変換 API を SDK に足す。

**上限とエラー。** 既存の sRGB 文書を P3 へ変える操作は作らない（COL-5 の変換で扱う）。canvas の `display-p3` が使えない環境では `createDocument` は成功し、表示だけ sRGB へ変換する。

## COL-2 16bit

**設計。** `PixelFormat::Rgba16` をタイル形式に追加する。Q14 の中間は16bitの入力を保持できないため、render v4 の新しい中間（Q30 を `rgba32sint`）を要する。GPU メモリは中間が2倍になり、現在の B6 の予算（512 MiB）に収まらない可能性が高い。

- タイルは 256×256×8 = 512 KiB。`alto-raster` の `TileData`、LZ4、ハッシュは形式に対して既に中立。
- ブラシの読み戻しは `rgba16unorm`。
- render v4 は3実装を新規に作る。v3 と同じ式を Q30 で書き直し、全モードを再固定する。

**判定条件。** 実装前に Q30 中間の試作で B1、B2、B6 を測る。4K・20層で B6 が 1 GiB を超えるなら、16bit は上限付きの文書（辺長 8192 まで）に限る。

## RV4-1 render v4 の基盤

**設計。** `DocumentMeta::render_version` を 3 または 4 にする。v3 は 8bit、gamma 合成の定義として残し、既存のハッシュを変えない。v4 は `depth: U16`、`depth: F16`、`blend_space: linear`、文字のヒンティングのいずれかを使う文書で使う。文書の作成時に決まり、途中で変えない（COL-5 の変換で別の文書を作る）。

- **中間。** Q30 を `rgba32sint`。WGSL に i64 はないので、積は既存の `effects-v3-wide.wgsl` の `Wide`（hi / lo の u32）と同じ形で計算する。
- **シェーダー。** 新規の `effects-q30.wgsl`、`composite-q30.wgsl`。`q14.wgsl` の式を Q30 で定義し、丸めは同じ規則にする。
- **参照。** `cpu_composite.rs` と `tests/e2e/cpu-composite.js` に v4 の分岐を加える。Rust は i128。JS は BigInt。
- **固定。** `tests/parity/v4-browser.txt`、`tests/parity/cpu-reference-v4.txt` を新設する。
- **予算。** 中間は1画素 16 byte で、v3 の2倍。`renderer/memory.rs` の `Governor` が見積もる一時ターゲットの量を形式ごとに分ける。

**順序。** Normal の合成だけで3実装を一致させてから、合成モード、マスク、効果、ベクター、テキストの順に移す。各段で全バイト一致の試験を通す。

## COL-3 F16 / HDR

**設計。** 浮動小数は GPU 間でビット一致せず、「正本は決定的」という前提と両立しない。そこで**正本は固定小数**とする。`depth: F16` の文書は `PixelFormat::Rgba16` に Q3.13（範囲 `[0, 8)`）で格納する。名前は既存の `BitDepth::F16` のままで、意味を文書化する。中間は RV4-1 の Q30 の範囲を `[−8, 8)` へ広げた Q27 とする。

表示は `display-p3` + `toneMapping: { mode: "extended" }`。非対応環境は 1.0 へクリップし、警告を出す。読み込みは Canvas の `rgba16float` の結果を固定小数へ量子化する。書き出しは 16bit PNG（1.0 でクリップ）と OpenEXR の最小のエンコーダー（無圧縮）を持つ。

## COL-4 リニア合成

**設計。** 文書属性 `blend_space: "gamma" | "linear"` を追加する。リニアは合成前に TRC を外し、合成後に戻す。8bit の TRC の往復はバンディングを生むため、Q14 では精度が足りず、RV4-1 の Q30 を前提にする。TRC は 4096 点の整数表を uniform のストレージに置き、3実装で同じ表を使う。

## COL-5 ICC カラーマネジメント

**設計。** 読み込み時に埋め込みプロファイルを文書の色空間へ変換し、保存時に標準プロファイルを埋め込む。D5 の決定により、依存を足さず自前で扱う。行列と TRC（`rXYZ`、`gXYZ`、`bXYZ`、`rTRC`、`gTRC`、`bTRC`）を先に、LUT（`A2B0`、`B2A0` の `mft1`、`mft2`、`mAB `、`mBA `）を COL-6 の段で追加する。

- 新規 crate `alto-color`：`parse_profile(bytes) -> Result<Profile, ColorError>`、`convert_rgba8(src: &[u8], from: &Profile, to: &Profile, intent: Intent) -> Vec<u8>`。演算は決定的な Q16 で、CLUT は整数の四面体補間。`Intent` は `perceptual`、`relative`（BPC なし）のみ。
- `engine.assets.importImage` の結果に `profile: { name, converted: boolean } | null` を追加する。

**上限。** プロファイル 4 MiB まで。不正なタグは `INVALID_ARGUMENT`。

## COL-6 CMYK、特色

**設計。** 文書の合成モデルは RGB が前提で、CMYK を正本にすると全経路が変わる。そこで**編集は RGB のまま**、次の3つを提供する。

- **ソフトプルーフ。** 表示経路だけに、文書→CMYK→表示の変換を 33³ の整数 3D LUT で適用する。ハッシュ対象の外で、`engine.viewport.setProof({ profile, intent } | null)` で切り替える。
- **CMYK 書き出し。** `exportImage({ type: "image/tiff", colorSpace: { cmyk: profile } })` で CMYK の TIFF（プロファイル埋め込み）を出す。
- **CMYK の読み込み。** JPEG と TIFF を読み、埋め込みまたは指定のプロファイルで文書の RGB へ変換する。元の値は保持しない（往復は無損失でない）。結果に警告を返す。

**特色。** レイヤーの属性 `spot: { name, lab: [L, a, b] } | null` を文書に持たせ、表示は Lab からの変換色で単色として合成する。書き出しは TIFF の追加チャネル（`spot` ごとの 8bit）とする。特色どうしの重ね刷りの結果は、表示では再現しない。この制限を文書に書く。

CMYK への編集の直接の操作（チャネル単位の編集）は提供しない。

## 実装タスク

### Task COL-1: Display P3 文書

**Files:** Modify `crates/alto-doc/src/lib.rs`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/types.ts`、`packages/alto-sdk/src/engine.ts`。Test `crates/alto-doc/src/lib.rs`、`tests/e2e/image-version-tests.js`。

**Interfaces:** `createDocument({ width, height, colorSpace?: "srgb" | "display-p3" })`。`snapshot.meta.color`。

- [ ] 試験 `display_p3_documents_validate_and_16bit_does_not`：P3 / U8 は成功、P3 / U16 と sRGB / F16 は `Invalid`。
- [ ] 試験 `p3_document_hash_matches_srgb_with_same_bytes`：同じ操作の P3 と sRGB の文書で、全タイルのハッシュが等しい。
- [ ] 実装し `sh scripts/check.sh native` が成功することを確認。
- [ ] ブラウザ試験 `p3 export carries the display-p3 color space`：書き出した PNG を `createImageBitmap` で読み、P3 の赤が sRGB の範囲外の値を持つ。
- [ ] 確定して PR。

### Task COL-5: 行列プロファイルの読み込み

**Files:** Create `crates/alto-color/src/lib.rs`。Modify `Cargo.toml`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/engine.ts`。

**Interfaces:** Produces `parse_profile`、`convert_rgba8`。

- [ ] 試験 `srgb_to_srgb_is_identity`：全 256³ の間引き（各チャネル 17 刻み）で入出力が一致。
- [ ] 試験 `lut_profile_is_refused_until_the_lut_stage`、`truncated_tag_is_refused`。
- [ ] 試験 `p3_red_maps_outside_srgb_clips_to_255`。
- [ ] 実装し、QA-1 のファザーに `parse_profile` を追加。
- [ ] 確定して PR。

### Task RV4-1: render v4 の基盤

**Files:** Create `crates/alto-gpu/shaders/effects-q30.wgsl`、`composite-q30.wgsl`、`crates/alto-gpu/src/composite_q30.rs`、`tests/parity/v4-browser.txt`、`tests/parity/cpu-reference-v4.txt`、`doc/verification/RV4-Q30-MEASURE.md`。Modify `crates/alto-core/src/lib.rs`、`crates/alto-doc/src/lib.rs`、`crates/alto-gpu/src/renderer.rs`、`renderer/m5.rs`、`cpu_composite.rs`、`tests/e2e/cpu-composite.js`。

**Interfaces:** `alto_core::RENDER_VERSION_Q30 = 4`。`target_format(exact)` を `target_format(version)` へ。

- [ ] Normal の合成で4K・20層の B1、B2、B6 を Metal で3回測り、判定条件を記録する。
- [ ] 試験 `v3_documents_keep_every_hash`（`git diff tests/parity` が空）。
- [ ] 試験 `q30_normal_matches_the_cpu_reference`（Metal、lavapipe、SwiftShader）。
- [ ] 試験 `render_version_cannot_change_in_a_transaction`、`v4_document_refuses_a_v3_merge`。
- [ ] 合成モード、マスク、効果、ベクター、テキストの順に PR を分けて移す。段ごとに parity を追加する。
- [ ] GPU 喪失後の再構築と、予算超過の明示エラーを試験する。

### Task COL-2: 16bit

**Files:** Modify `crates/alto-core/src/lib.rs`（`PixelFormat`）、`crates/alto-raster/src/lib.rs`、`crates/alto-store/src/lib.rs`、`crates/alto-engine/src/image.rs`、`canvas.rs`、`crates/alto-gpu/shaders/brush.wgsl`、`packages/alto-sdk/src/engine.ts`、`worker.ts`。

**Interfaces:** `createDocument({ depth: "u8" | "u16" })`、`PixelFormat::Rgba16`、`exportImage` の 16bit PNG。Consumes RV4-1。

- [ ] 試験 `rgba16_tile_roundtrips_and_hashes`、`mixing_8_and_16_bit_tiles_in_a_document_is_refused`。
- [ ] 試験 `image_edits_keep_16_bits`（resize、gradient、fill、merge の各操作で下位バイトが保たれる）。
- [ ] ブラウザ試験 `a 16 bit stroke commits through readback`、`a 16 bit PNG imports and exports without loss`。
- [ ] 保存、再オープン、コミット、差分、マージの往復を試験する。
- [ ] 実装、PR。

### Task COL-4: リニア合成

**Files:** Modify `crates/alto-doc/src/lib.rs`、`crates/alto-gpu/shaders/composite-q30.wgsl`、参照の2実装。

**Interfaces:** `createDocument({ blendSpace: "gamma" | "linear" })`。

- [ ] 試験 `trc_table_roundtrip_error_is_at_most_one_q30_step_per_16_bit_value`。
- [ ] parity ケース `linear-normal`、`linear-multiply`、`linear-blur`。
- [ ] 試験 `blend_space_requires_render_v4`。
- [ ] 実装、PR。

### Task COL-3: F16 / HDR

**Interfaces:** `createDocument({ depth: "f16" })`。Consumes RV4-1、COL-2。

- [ ] 試験 `values_above_one_survive_compositing`、`negative_results_clamp_at_zero_in_the_canonical_tile`。
- [ ] ブラウザ試験 `a display without extended range clips and warns`。
- [ ] 試験 `exr_header_is_valid_and_pixels_roundtrip`。
- [ ] 実装、PR。

### Task COL-6: LUT、ソフトプルーフ、CMYK、特色

**Files:** Modify `crates/alto-color/src/lib.rs`、`crates/alto-store/src/tiff.rs`（STO-3）、`crates/alto-gpu/shaders/present-q14.wgsl`、`present-q30.wgsl`、`packages/alto-sdk/src/viewport.ts`、`document.ts`、`crates/alto-doc/src/lib.rs`（`spot`）。

**Interfaces:** `convert_cmyk_to_rgb8`、`convert_rgb8_to_cmyk`、`engine.viewport.setProof`、`LayerProps.spot`。

- [ ] 試験 `tetrahedral_at_grid_points_returns_the_table_values`、`mft2_and_mab_parse_the_same_lut`。
- [ ] 実在のフリーの CMYK プロファイルを確保し、ライセンスを `THIRD-PARTY-NOTICES.md` へ記載する。商用プロファイルは同梱しない。
- [ ] 試験 `proof_does_not_change_any_tile_hash`。
- [ ] 試験 `cmyk_tiff_has_the_right_tags_and_embedded_profile`、`spot_channels_are_written_per_layer`。
- [ ] ブラウザ試験 `proof toggling keeps the document clean`。
- [ ] 実装、PR。

## Review Focus

- 非対応画面の P3 文書で、表示が黙って色ずれしないこと。警告を出す。
- P3 の文書へ sRGB の画像を読み込んだとき、バイトをそのまま解釈しないこと。
- プロファイル付き JPEG の読み込みで、変換の有無を結果に残すこと。
- 色入力が文書の色空間であることが SDK の型の文書から読めること。
- 異なる色空間の文書間のマージは `UNRELATED_HISTORY` として拒否されること。
