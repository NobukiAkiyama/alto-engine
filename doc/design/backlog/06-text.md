# テキスト

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 日本語組版の残りと、書式・フォント機能を追加する。

**Architecture:** `alto-text` が組版を行い、結果を `TextBake`（整数の字形配置とアウトライン）に焼く。render v3 は焼き込みから描くので、組版だけの変更は描画の3実装を変えない。`tests/parity/text-bake.txt` の既存行は不変、新しい機能は新しい行を持つ。

## 現状

- `crates/alto-text/src/model.rs`：`TextStyle { fonts, size, color, line_height, letter_spacing, scale, features, language }`、`StylePatch`、`TextSpan`、`TextAlign`（全体に1つ）、`TextFrame`。
- `crates/alto-text/src/layout.rs`（849行）：段落→書記素→ラン。禁則は追い出し。
- 上限：UTF-16 16384、span 1024、フォールバック 16、機能 64。
- 焼き込み：`crates/alto-engine/src/text.rs` の `bake_text`、`bake_ops`。

## データモデル

```rust
pub struct TextContent {
    // 既存のフィールドに追加
    pub paragraphs: Vec<ParagraphRange>,  // TXT-5
    pub ruby: Vec<RubyRange>,             // TXT-1
    pub emphasis: Vec<EmphasisRange>,     // TXT-2
    pub warichu: Vec<TextRange>,          // TXT-3
    pub hanging: bool,                    // TXT-4
}
pub struct ParagraphRange { start: u32, end: u32, align: Option<TextAlign>, indent_first: f32, indent_start: f32, indent_end: f32 }
pub struct RubyRange { start: u32, end: u32, text: String, kind: RubyKind /* mono | group | jukugo */, align: RubyAlign, overhang: bool }
pub struct EmphasisRange { start: u32, end: u32, mark: EmphasisMark /* sesame | dot | circle | triangle | custom(char) */, side: Side, filled: bool }
```

`StylePatch` と `TextStyle` に `underline: Option<Underline>`（TXT-6）、`italic_skew: f32`（TXT-7）、`variations: Vec<FontVariation>`（TXT-8）を追加する。

範囲はすべて UTF-16。サロゲートの途中は拒否。編集時の範囲の追従は `TextSpan` と同じ規則（挿入で伸び、削除で縮み、空は消す）を共通関数へ移す。

## TXT-4 ぶら下げ

**設計。** 行末禁則で句読点を追い出す代わりに、枠の外へ1字ぶら下げる。`hanging: true` のとき、次行頭に来る句読点（`、。，．`）を行末に残し、行の長さの判定から外す。縦横とも。揃えの Justify ではぶら下げ字を除いて均等にする。

## TXT-5 字下げ、段落ごとの揃え

**設計。** 段落は改行で区切る既存の単位。`ParagraphRange` は段落の属性を持ち、本文の編集で段落の境界に追従する。`indent_first` は em。折り返し幅の計算と行頭の位置を段落ごとに変える。全体の `align` は既定値として残す。

## TXT-6 下線

**設計。** フォントの `post.underlinePosition`、`underlineThickness` から計算する。縦組みは右側の傍線。線は `TextBake` の字形とは別の矩形レコードとして焼く。矩形のカバレッジは字形と同じ整数パスに載せる（4点の Move/Line/Close のアウトラインとして焼く）。描画の3実装は変わらない。

## TXT-7 斜体の合成

**設計。** `italic_skew`（度、−30〜30）を字形の行列に加える。横組みはせん断 `x' = x + y tan θ`。縦組みの漢字はせん断の方向を y へ変える。焼き込みの字形変換 `t: [f64; 6]` に入るため、描画の変更はない。

## TXT-8 可変フォント

**設計。** rustybuzz の `set_variations` と ttf-parser のアウトライン補間を使う。`FontVariation { axis: [u8; 4], value: f32 }`、16 個まで。軸の範囲外は clamp せず拒否する。字形アトラスの鍵とアウトラインキャッシュの鍵に軸値（Q16）を追加する。`crates/alto-core` の `BakeFont` に同じ値を載せ、焼き込みのバイトが変わらないよう、軸なしのときは既存の形のままにする。

## TXT-1 ルビ

**設計。** 親文字の外側に、親の 50% のサイズで配置する。

- モノルビ：親の1字ごと。グループルビ：範囲全体に均等配置。熟語ルビ：字ごとで、はみ出しは隣の親字へ。
- 親より長いルビは親の字間を広げる（JLREQ の既定）。`overhang: true` のときは隣の仮名の上へ最大 1/2 字かける。
- 行送りの計算：行の外側に余白が足りなければ、行送りを増やす。増やした分は行ごとに持つ。
- 焼き込み：ルビの字形は親のランと別の `TextRun` として焼く。アトラスと経路の選択は既存のサイズ判定に従う。
- 縦組みは親の右側。横組みは上。`TextInfo` にルビの範囲を出す。
- 選択とカーソルはルビの文字に入らない。

## TXT-2 圏点

**設計。** 親字の中心の外側に記号字形を置く。字形の出所はフォントの該当字（`﹅` など）を優先し、無ければ fallback の順に探す。どれにも無ければ円のパスを合成する。

## TXT-3 割注

**設計。** 指定範囲を親の半分のサイズで2行に分けて行内へ流す。行をまたぐ場合は分割する。最も複雑で、行分割の不変条件を変える。TXT-1 の後に、`layout.rs` の行分割を「インラインボックス」の列へ整理してから入れる。段階3。

## TXT-9 カラーフォント

**設計。** `COLR` v0 のレイヤーを、字形ごとのパスと色の列へ展開して焼く。単色字形の経路を色ごとに複数回通すので、新しいシェーダーは要らない。`sbix` と `CBDT` のビットマップ字形は表示サイズに依存するので、ラスターのタイル化を要する。第一段階は COLR v0 のみ。COLR v1（グラデーション）は VEC-1 の後。

## TXT-10 ヒンティング、ステム強調、ガンマ、LCD

**設計。** render v3 の定義を変えるため render v4 のみ（RV4-1 の上）。文書属性 `text_rendering: { hint_y: bool, stem_darken: f32, coverage_gamma: f32 }` を持つ。

- **ヒンティング。** y 方向の整数スナップ（`2026-10-28-v3-integer-text.md` の未決事項4）。TrueType の命令は実行しない。ベースラインと x-height を出力画素に揃える整数の変換を、焼き込みの字形座標に適用する。
- **ステム強調。** アウトラインを整数でオフセットする（表示サイズ 24px 未満）。
- **ガンマ補正。** カバレッジを 256 点の整数表で変換してから合成する。
- **LCD。** 下地が不透明でないと成立せず、編集可能な合成と両立しない。表示経路の専用処理（ハッシュの対象外）とし、ノードの直下が不透明と判定できる場合だけ、提示の段で3倍のカバレッジを使う。表示タイルのキャッシュを別に持つため、最後に実装し、測定で高価なら既定を無効にする。

## TXT-11 縦組み中の RTL

**設計。** 縦組みの中のアラビア文字やヘブライ文字の連続を、1つのランとして RTL でシェーピングし、90 度回転して上から下へ置く。`orient.rs` の分類と、行内の bidi レベルを組み合わせる。今は字ごとに回転するだけで、連続の並びが逆になる。

## 実装タスク

### Task TXT-4 / 6 / 7: 小さな書式

**Files:** Modify `crates/alto-text/src/model.rs`、`layout.rs`、`crates/alto-engine/src/text.rs`、`packages/alto-sdk/src/types.ts`、`packages/alto-sdk/src/document.ts`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`。

**Interfaces:** `TextContent.hanging`、`StylePatch.underline`、`StylePatch.italic_skew`。

- [ ] 試験 `hanging_keeps_the_period_on_the_line`、`hanging_does_not_count_in_justify`。
- [ ] 試験 `underline_uses_the_font_metrics`、`vertical_underline_is_on_the_right`。
- [ ] 試験 `skew_zero_bakes_the_same_bytes`。
- [ ] 既存の焼き込み行が不変であることを確かめ、PR。

### Task TXT-5: 段落属性

**Interfaces:** Produces `ParagraphRange`、`doc.text.setParagraph(id, start, end, patch)`、範囲追従の共通関数 `fn remap_range(range, edit) -> Option<Range>`。

- [ ] 試験 `paragraph_align_overrides_the_default`、`first_line_indent_shortens_the_first_line`。
- [ ] 試験 `editing_text_keeps_paragraph_attributes`、`deleting_a_newline_merges_into_the_first_paragraph`。
- [ ] 試験 `set_text_is_one_undo`、保存の往復。
- [ ] 実装、PR。

### Task TXT-1 / TXT-2: ルビと圏点

**Interfaces:** Consumes `remap_range`。Produces `RubyRange`、`EmphasisRange`、`doc.text.setRuby`、`doc.text.setEmphasis`。

- [ ] 試験 `mono_ruby_is_centered_on_each_base`、`group_ruby_spreads_over_the_range`、`long_ruby_widens_the_base`。
- [ ] 試験 `ruby_increases_the_line_advance_when_needed`。
- [ ] 試験 `vertical_ruby_is_on_the_right`、`ruby_does_not_enter_the_selection`。
- [ ] 試験 `ruby_range_cannot_cross_a_newline`、`emphasis_falls_back_to_a_drawn_circle`。
- [ ] GPU 画素試験（`m7-tests.js`）`ruby renders at its bake position`、`GPU loss rebuilds ruby`。
- [ ] 実装、PR。

### Task TXT-8: 可変フォント

- [ ] 試験 `variation_changes_the_outline`、`axis_out_of_range_is_refused`、`no_variation_bakes_the_same_bytes`。
- [ ] 試験 `atlas_key_differs_per_axis_value`。
- [ ] テスト用の小さな可変フォントを `tests/fonts` に OFL ライセンスで追加し、`THIRD-PARTY-NOTICES.md` を更新。
- [ ] 実装、PR。

### Task TXT-3、TXT-9、TXT-11

- [ ] TXT-11：試験 `rtl_run_in_vertical_keeps_its_order`。
- [ ] TXT-3：先に `layout.rs` のインラインボックス化を単独の PR で行い、`text-bake.txt` の不変を確かめる。その上で試験 `warichu_splits_into_two_lines`、`warichu_breaks_across_lines`。
- [ ] TXT-9：試験 `colr_v0_draws_layers_in_order`、`sbix_only_font_warns_and_draws_the_fallback_box`。

### Task TXT-10: 文字の描画品質（render v4）

**Files:** Modify `crates/alto-core`（`text_bake`）、`crates/alto-engine/src/text.rs`、`crates/alto-gpu/src/glyph_q8.rs`、`glyph_atlas.rs`、`shaders/glyph-q14.wgsl` の v4 版、参照の2実装。

- [ ] 試験 `v3_bake_bytes_are_unchanged`。
- [ ] parity ケース `text-v4-hint`、`text-v4-stem`、`text-v4-gamma`。
- [ ] 試験 `hinting_keeps_advances`（y のみを変え、組版の位置を変えない）。
- [ ] LCD の試験 `lcd_is_off_over_transparency`、`lcd_never_changes_a_hash`、書き出しで LCD が使われないこと。
- [ ] 実装、PR。

## Review Focus

- 範囲の追従で、ルビの親文字が削除されたらルビも消えること。
- 焼き込みの最大サイズ。ルビと圏点の字形で `TextBake` の上限を超える文書を拒否できること。
- `merge` の対象。テキストの差分が新しいフィールドを見ること（`version.rs` の `DiffItem`）。
- 既存のカーソル移動が、ルビなどの追加で UTF-16 の添字を変えないこと。
- フォント削除の検査が、ルビや圏点のフォント参照を含むこと。
