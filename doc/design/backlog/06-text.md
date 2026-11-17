# テキスト

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 日本語組版の残りと、書式・フォント機能を追加する。

**Architecture:** `alto-text` が組版を行い、結果を `TextBake`（整数の字形配置とアウトライン）に焼く。render v3 のテキストは焼き込みだけから描く（`RContent::TextBake`。`crates/alto-gpu/src/renderer/tile_renderer/graph.rs`）ので、組版だけの変更は描画の3実装（CPU 参照、JS 参照、WGSL）を変えない。`tests/parity/text-bake.txt` の既存行は不変、新しい機能は新しい行を持つ。描画の定義を変える TXT-10 だけが render v4 で、3実装に触れる。

## 共通の現状

- 型（`crates/alto-text/src/model.rs`）：`TextStyle { fonts, size, color, line_height, letter_spacing, scale, features, language }`、`StylePatch`（`line_height` を除く同じ項目の `Option`）、`TextSpan { start, end, style }`、`TextRange`、`TextAlign`（`Start | Center | End | Justify`。`TextContent.align` に全体で1つ）、`TextFrame`、`TextContent { text, style, spans, frame, writing_mode, align, tcy, auto_tcy }`。すべて `deny_unknown_fields`。範囲は UTF-16 で、`Utf16Index` が文字境界を検査する（サロゲートの途中は `byte()` が `None`）。
- 検証：`TextContent::validate` が `Err(TextError)` を返し、`crates/alto-doc/src/document.rs` の `Document::validate_graph` が `DocError::Invalid`（コード `INVALID_ARGUMENT`）にして、`font_ids()` の各フォントが `AssetKind::Font` であることも調べる。上限：本文 16384 UTF-16（`MAX_TEXT_UTF16`）、span と縦中横が各 1024（`MAX_SPANS`）、フォールバック 16（`MAX_FONTS`）、機能 64（`MAX_FEATURES`）、縦中横 1 組 8 UTF-16（`MAX_TCY_UTF16`）。フレームは各辺 1048576 以下。堅牢化 A〜F（`doc/verification/ENGINE-HARDENING-RESULTS.md`）はこれらの上限を変えていない。
- 範囲の追従：`TextContent::replace_range` が spans と縦中横を編集に合わせて動かす。Rust 側の呼び出し元は自分の試験だけで、実際の編集は SDK の `replaceText`（`packages/alto-sdk/src/text-edit.ts`）が同じ規則を JavaScript で実装し、`doc.text.replace` が `setText` で全文を送る。規則は「境界での挿入は直前の span に属する」「選択を置き換えるときは置き換えた文字の書式を保つ」「縦中横の内部の編集は組を解く」。
- 組版（`crates/alto-text/src/layout.rs`、849 行）：`layout` が改行で段落に分け、`shape_paragraph` が書記素 → ラン（書式、フォント、bidi レベル、向き）→ rustybuzz → `Cluster` の列にし、UAX #14 の行分割機会に日本語の禁則（`no_line_start`、`no_line_end`。`breaks` を偽にして追い出す）を重ね、貪欲に行を切る。行の太さは `extent = size × line_height`（行内の最大）と `asc + desc` の大きいほうで、`place` が `PlacedGlyph { glyph, transform: [f64; 6], color, cluster, size }` を作る。縦組みの bidi は全文字をレベル 0 にしている。
- 焼き込み：`Engine::bake_text`、`Engine::bake_ops`（`crates/alto-engine/src/text.rs`）が `layout` の `glyphs` を `TextBake`（`crates/alto-core/src/text_bake.rs`。`fonts`、`outlines`、`glyphs`）にする。`BakeGlyph { outline, m: [i32; 4]（Q24）, t: [i64; 2]（Q30）, color: [u16; 4]（Q14） }`、`BakeOutline { font, glyph, upem, commands（Q6） }`。輪郭は `bake_text` の `(Option<AssetId>, glyph)` で重複を除く。`BakeFont` はフォントの hash と face の記録で、描画は読まない（`glyph_bitmaps`、`tests/e2e/cpu-composite.js` の `decodeTextBake` とも `fonts` を使わない）。ベイクは `Op::SetText { id, content, bake }` に載り、受け取ったベイクは再計算しない（描画は `TextBake::decode` で全範囲を検査し、壊れていれば拒否する）。
- 描画：`crates/alto-gpu/src/glyph_q8.rs` の `glyph_map_q` が焼き込みの `m`、`t` にノードの変換とレベルを合成し、`atlas_band`（em が 24 px 未満）で字形アトラス（`atlas_key` は輪郭のバイト列の blake3 で、`(font, glyph)` ではない）か直接ラスタ化かを選ぶ。
- 固定：`tests/parity/text-bake.txt` は3行（`text-v3-latin`、`text-v3-vertical`、`text-v3-placeholder`）。`crates/alto-engine/src/text_bake_tests.rs` が焼いて比べ、`ALTO_WRITE_PARITY=1` で再生成する。同じ試験が `rustybuzz`、`ttf-parser`、`unicode-*` のロック版を固定している（`text_bake_dependencies_are_pinned`）。同じ入力が別のバイトに焼かれるときは `BAKER_VERSION`（`crates/alto-engine/src/text.rs`）を上げて理由を書く。
- SDK：`TextContent`、`StylePatch`（`packages/alto-sdk/src/types.ts`）、`doc.text.{set, replace, style, setTateChuYoko, info, hitTest, caret, selectionRects, navigate}`（`packages/alto-sdk/src/document.ts`）、意図の名前（`packages/alto-sdk/src/intents.ts`）、API 報告（`packages/alto-sdk/api/alto-engine.api.md`、`alto-engine-browser.api.md`）。Worker の要求は `packages/alto-sdk/src/protocol.ts`（`PROTOCOL` は現在 8）、WASM の入口は `crates/alto-wasm/src/text.rs`。
- 差分：`crates/alto-doc/src/version.rs` の `content_change` が、テキストノードの内容が違うとき `ContentDetail::Text { text, style, writing_mode, frame, tate_chu_yoko }` を作る。`crates/alto-git/src/lib.rs` の `describe` がこれを日本語にする。内容が違うのにどのフラグも立たない変更は、空の説明になる。マージ（`crates/alto-engine/src/versions/merge.rs`）はノードを JSON の値として3方向で比べ、内容の食い違いを `content` の競合にする。テキスト固有の処理は無い。
- 試験の場所：組版は `crates/alto-text/tests/layout.rs`（フォントは `tests/fonts/AltoTestJP.otf`、`AltoTestLatin.ttf`。どちらも `post` を持ち、下線は Latin が位置 −100・太さ 50、JP が位置 −125。ヘブライ文字の字形は無く、既存の bidi の試験は欠けた字形のまま並びだけを見る）、焼き込みは `crates/alto-engine/src/text_bake_tests.rs`、エンジン経由は `crates/alto-engine/src/text_tests.rs`、描画は `crates/alto-gpu/src/cpu_composite/text_tests.rs`、ブラウザは `tests/e2e/m7-tests.js`（名前は古いが現役）と `tests/e2e/parity-tests.js`。

## 共通の設計

### 追加するフィールドの扱い

- `TextContent` に足すフィールドはすべて `#[serde(default, skip_serializing_if = ...)]` とし、既定値のときは JSON に出さない。既存の文書は保存、ジャーナルのダイジェスト、`text_entry` の鍵（`serde_json` で直列化した内容）が1バイトも変わらない。試験 `new_text_fields_serialize_as_before`（新規）（既存の3つの parity テキストの直列化が追加前と同じ）で固定する。
- 新しいフィールドは `deny_unknown_fields` の型へ入るので、旧版は新しい文書を開けない。共通規則3により `format_version` を上げる（値は要割り当て）。同じ時期の PR は1回の番号にまとめる。案：TXT-4、5、6、7 を1回、TXT-1、2 を1回、TXT-8 を1回、TXT-3 を1回。TXT-10 の文書属性は RV4-1 の `render_version` と同じ回にする。
- `format_version` を上げるときは、パッケージの manifest も変える。`crates/alto-store/src/lib.rs` は `document_format: 5` を固定で書き、読み込みは 5 以外を `StoreError::Unsupported("package format")` で拒否する。したがって各回の作業手順に、書き出す値と受け入れる値の更新（`alto-store` の定数化を含む）、`crates/alto-doc/src/lib.rs` の `DocumentMeta` の 5 の更新、`document.rs` の形式の試験の更新を入れる。
- `PROTOCOL` は、新しい要求（TXT-8 の `fontAxes`（新規））を足すときだけ上げる（値は要割り当て）。`setText` の内容と `textInfo` の応答に任意のフィールドを足すだけの項目は上げない。
- ALTB（焼き込みの形式）は TXT-1〜9、11 で変えない（`BAKER_VERSION` を上げるのは、既存の入力の出力が変わる TXT-11 だけ）。ルビ、圏点、下線、色層はすべて既存の `BakeGlyph`（輪郭への参照、行列、位置、色）で表せる。したがって `decodeTextBake`（JS 参照）、`TextBake::decode`、WGSL は触れない。`BAKER_VERSION` も、既存の入力の出力が変わらないので上げない。

### 範囲の追従

remap_range（TXT-5 で新設。下の型は新規）を Rust（`model.rs`）と TypeScript（`text-edit.ts`）の両方に置く。

```rust
pub struct Edit { pub start: u32, pub end: u32, pub added: u32 }   // [start, end) を added 個の UTF-16 に置き換える
pub enum RangePolicy { Style, Group }
pub fn remap_range(r: TextRange, e: Edit, p: RangePolicy) -> Option<TextRange>;
```

- `Style`（span、圏点、割注）：今の `replace_range` の span の規則をそのまま写す（境界の挿入は直前の範囲を伸ばす、選択の置き換えは範囲の始点を保つ、空になったら `None`）。
- `Group`（縦中横、ルビ）：編集が範囲と重なるか、範囲の内側に挿入するときは `None`（組が解ける）。境界での挿入は範囲に入らず、範囲は平行移動だけする。親文字が削除されるとルビも消える。
- 試験：`remap_range_matches_the_old_span_rules`（`replace_range_moves_spans` の期待値を表にして共通に使う）。表は `tests/fixtures/text-remap.json`（新規）に置き、Rust と SDK の両方が読む。2つの実装が食い違わないことを固定する。

### 上限（新規の定数）

`model.rs` に置く。超えたら `TextContent::validate` が `TextError` で拒否し、`INVALID_ARGUMENT` になる。

| 定数 | 値 | 内容 |
|---|---|---|
| `MAX_PARAGRAPH_ATTRS`（新規） | 1024 | 段落属性の数 |
| `MAX_RUBY`（新規） | 1024 | ルビの組の数 |
| `MAX_RUBY_BASE_UTF16`（新規） | 64 | 1組の親文字 |
| `MAX_RUBY_TEXT_UTF16`（新規） | 64 | ルビ1項目の長さ |
| `MAX_RUBY_TOTAL_UTF16`（新規） | 8192 | ルビ文字の合計 |
| `MAX_EMPHASIS`（新規） | 1024 | 圏点の範囲の数 |
| `MAX_WARICHU`（新規） | 256 | 割注の数 |
| `MAX_WARICHU_UTF16`（新規） | 512 | 1組の割注 |
| `MAX_VARIATIONS`（新規） | 16 | 1スタイルの可変軸 |

### 焼き込みの上限

`bake_text` は今、字形が範囲外のとき黙って飛ばし、字形数に上限がない。ルビ、圏点、色層で字形が増えるので、`alto-engine` の `text.rs` に上限を足す。

- `MAX_BAKE_GLYPHS`（新規）= 65536。本文 16384 UTF-16 の字形、ルビ最大 8192、圏点最大 16384、色層、下線の矩形を合わせた最悪が収まる値。
- `MAX_BAKE_BYTES`（新規）= 32 MiB（案）。既定は案で、実装時に 16384 字の漢字だけの文書（`tests/fonts/subset-text.txt` の字を繰り返す）を焼いたバイト数を測り、その2倍以上に決める。
- 超えたら変更全体を拒否する。コードは `RESOURCE_LIMIT`（`crates/alto-engine/src/canvas.rs` が使う既存のコード）。そのため `bake_ops` と `bake_text` を `Result` にし、`bake_text_ops`（`crates/alto-engine/src/lib.rs`）が `?` で返す。`transact` は焼いてから適用するので、失敗は状態を変えない。
- 受け取ったベイク（他の端末の Op）は再計算も新しい上限検査もしない既存の方針を保つ（描画時の `TextBake::decode` の検査は今のまま）。
- `transact` は焼いてから適用するので、拒否されても文書の状態は変わらないが、それまでに作った blob は残りうる（孤立した blob）。試験は文書の状態（改訂、内容、Undo の深さ）が変わらないことを確かめ、blob には触れない。

### 焼き込みの順序

`layout` が返す `glyphs` を、本文、ルビ、圏点の順に並べ、`rules`（TXT-6）はその後ろに矩形として焼く。ルビなどが無い文書の `glyphs` は今と同じ列なので、既存のハッシュは変わらない。

## TXT-1 ルビ

### 現状

- ルビの型も配置も無い。`crates/alto-text/src/model.rs` の `TextContent` にルビの項目は無い。
- 行の太さを決めるのは `layout` の `extent`、`asc`、`desc`、`thickness`（`crates/alto-text/src/layout.rs`）。ブロック方向の位置は `block` で、横組みは上から下へ、縦組みは右から左へ進む。余白は `thickness` と `asc + desc` の差を上下で半分ずつ持つ（`baseline = r[0] + (thickness - asc - desc) / 2 + asc`）。
- `place` は `Cluster` ごとに `PlacedGlyph` を作る。`PlacedGlyph.cluster` は `out.clusters` の添字で、選択とキャレット（`crates/alto-text/src/select.rs` の `hit_test`、`caret`、`selection_rects`、`navigate`）は `ClusterBox` だけを見る。
- `TextInfo`、`LineInfo`（`crates/alto-engine/src/text.rs`）と SDK の `TextInfo`（`types.ts`）。
- 焼き込みには大きさの概念が無い。ランをサイズで分けるのは `glyph_runs`（`crates/alto-text/src/outline.rs`）の `TextRun` で、`text_entry` の情報用（`TextInfo.paths`）にだけ使う。v3 の描画は字形ごとの `atlas_band`（行列から求めた em が 24 px 未満か）でアトラスか直接ラスタ化かを選ぶ。

### 11-04 からの変化

- 旧設計は「ルビの字形は親のランと別の `TextRun` として焼く。アトラスと経路の選択は既存のサイズ判定に従う」と書いた。v3 の焼き込みに `TextRun` は無い。ルビは小さな `m` を持つ普通の `BakeGlyph` で、サイズ判定は描画側が行列から自動的に行う。この記述を削除し、焼き込みは変えないことにした。
- `layout.rs`、`model.rs`、`text.rs` は 11-04 以降、`f8394d1`（フォント集合のヘッダ検査）と `4121d69`（モジュールの改名）以外に変わっていない。組版の構造は変化なし。
- 旧設計の「`TextInfo` にルビの範囲を出す」は残す。

### 設計

**データ。**

```rust
pub enum RubyKind { Mono, Group, Jukugo }
pub enum RubyAlign { Center, Start, Distribute, Edge }   // 中付き、肩付き、均等割り（1-2-1）、両端揃え
pub struct RubyRange {
    pub start: u32, pub end: u32,       // 親文字（UTF-16）
    pub text: Vec<String>,              // Group は1項目。Mono、Jukugo は親の書記素と同数（空文字は「その字にルビ無し」）
    pub kind: RubyKind,
    #[serde(default)] pub align: Option<RubyAlign>,   // None は kind の既定（Mono、Jukugo は Center、Group は Distribute）
    #[serde(default)] pub overhang: bool,
}
// TextContent.ruby: Vec<RubyRange>  — 始点順、重ならない
```

- 検証：親は空でなく、MAX_RUBY_BASE_UTF16 以下で、改行を含まない。サロゲートの途中は拒否。各項目は MAX_RUBY_TEXT_UTF16 以下で制御文字を含まず、合計が MAX_RUBY_TOTAL_UTF16 以下（定数は上の表）。`Mono`、`Jukugo` で `text.len()` が親の書記素の数と違えば拒否。縦中横と重なる場合は、縦中横が親に完全に含まれるときだけ許し、部分的な重なりは拒否する。割注との重なりの検査は、`warichu` を足す TXT-3 の検証に置く（TXT-1 の時点では割注が無い）。
- 範囲の追従は `RangePolicy::Group`。
- ルビは親のスタイルを使う。`size` を `RUBY_SIZE_RATIO`（新規）の 0.5 倍にし、`letter_spacing` は 0、`fonts`、`features`、`language`、`color`、`scale` は親と同じ。親の途中でスタイルが変わるときは、ルビ1項目ごとに、対応する親の先頭文字のスタイルを使う。ルビ専用のスタイルは持たない（フォントの参照が増えないので、フォント削除の検査は `font_ids()` のままでよい）。

**配置（`layout.rs`）。**

1. `shape_paragraph` の後、行分割の前に、各ルビの項目をルビのスタイルで整形し（`shape_paragraph` が `it.style_at(abs)` で本文のバイト位置からスタイルを引くので、整形の入口を「スタイルの出所」を引数に取る形へ分ける。TXT-3 の下準備と同じ変更）、幅 `w_r` を得る。親の `Cluster` の送りの合計を `w_b` とする。
2. 親より長いルビ（`w_r > w_b`）の不足 `d = w_r - w_b`：
   - `overhang: true` のとき、前後の親でない仮名（ひらがなかカタカナで、ルビを持たない）の上へ、片側 `min(0.5 em, その字の送り)` まで（`RUBY_OVERHANG_MAX_EM`（新規）= 0.5 em）かけてよい。かけた分を `d` から引く。
   - 残りは親の字間を広げる（JLREQ の既定）。`Mono`：その字の `Cluster` の送りを `d` だけ増やし、字形は `d/2` ずらす（`Cluster.lead`（新規）フィールドを `place` が `pen` に足す）。`Group`：各親字へ `d / n` を等分し、前後に半分ずつ（均等割り）。`Jukugo`：字ごとのルビを `Mono` と同じに配るが、隣のルビの余白（その字の `b_j - w_j` の半分まで）には食い込んでよい。足りなければ字間を広げる。
3. `Group`、`Jukugo` の親の内部では行分割の機会を禁じる（`allow_break` を偽にする）。親が行より長いときは、単一のクラスターが行より広い場合と同じく、はみ出す。
4. 行の組み終わりの後（`place` の後）に、親の `ClusterBox.inline` の範囲から親の左右（縦組みは上下）の端を求め、ルビの `align` に従って置く。`Center`：親の幅の中央。`Start`：端へ。`Distribute`：ルビの文字数 `m` に対し、余白 `(w_b - w_r) / m` の半分を両端に、残りを字間に置く（`m = 1` は中央）。`Edge`：余白を字間だけに置く。親の並びが bidi で逆でも、実際の位置から求めるので同じ式でよい。
5. ルビのブロック方向の位置：横組みはルビの下端を親の字形の上端（`asc` の高さ）へ合わせ、縦組みはルビの左端を親の右端へ合わせる。ルビの高さは `size × RUBY_SIZE_RATIO × (asc + desc) / upem`。
6. 行の太さ：ルビが本文の余白に収まらなければ、その行だけ広げる。`need = ルビの高さ`、`have = (thickness - asc - desc) / 2`、`extra = max(0, need - have)`。ブロック範囲を `extra` だけルビ側へ広げ、ベースラインもその分ずらす（横組みは上へ、縦組みは右へ）。ルビの無い行は `extra = 0` で今と同じ。`LineBox` に `ruby_extra: f64`（新規）を持たせる。次の行の位置は広がった分だけ進む。
7. `PlacedGlyph` はルビも本文と同じ型で、`cluster` は対応する親の先頭の `Cluster` の添字にする。`ClusterBox` は増やさないので、ヒットテスト、キャレット、選択、`navigate`、`LineBox.clusters` は変わらず、ルビの文字には入れない。
8. 焼き込み（`bake_text`）は `glyphs` の順に焼く。ルビは本文の後ろ。

**公開。** `TextLayout.ruby: Vec<RubyBox>`（新規）（`start`、`end`、`rect: [f64; 4]` ノード座標）。`TextInfo.ruby`（新規）に出し、SDK の `TextInfo` に任意のフィールド `ruby?: { start, end, rect }[]` を足す。

**SDK。** `doc.text.setRuby(id, start, end, spec | null)`（`null` で解除。範囲と重なる既存のルビを置き換える）。意図は `"text.ruby"`（`intents.ts` に追加）。

### 形式と通信

- `format_version`：上げる（TXT-1、2 で1回。値は要割り当て）。`deny_unknown_fields` の `TextContent` に `ruby` が加わる。
- `PROTOCOL`：上げない。`setText` の内容と `textInfo` の応答に任意のフィールドを足すだけ。
- ALTB と描画：変えない。render v3。`text-bake.txt` に新しい行 `text-v3-ruby-horizontal`、`text-v3-ruby-vertical`（新規）を足す。既存3行は不変。
- 差分：`ContentDetail::Text` に `ruby: bool`（新規）を足し、`describe`（`alto-git`）に「ルビ」を足す。

### 作業手順

**Files:** Modify `crates/alto-text/src/model.rs`、`layout.rs`、`crates/alto-engine/src/text.rs`、`crates/alto-doc/src/version.rs`、`crates/alto-git/src/lib.rs`、`packages/alto-sdk/src/types.ts`、`document.ts`、`text-edit.ts`、`intents.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`alto-engine-browser.api.md`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`、`examples/browser-playground/text-ui.js`（`TextContent` を作り直す箇所が新しいフィールドを落とさないこと）。確認のみ：`crates/alto-text/src/select.rs`（ルビを足しても出力が変わらないこと）。

**Interfaces:** Consumes `remap_range`（TXT-5）。Produces `RubyRange`、`doc.text.setRuby`、`TextLayout.ruby`。

- [ ] 先に `shape_paragraph` のスタイルの出所を引数にする単独の変更を入れ、`cargo test -p alto-text -p alto-engine` と `tests/parity/text-bake.txt` が不変であること。
- [ ] 試験（`crates/alto-text/tests/layout.rs`）`mono_ruby_is_centered_on_each_base`、`group_ruby_spreads_over_the_range`、`long_ruby_widens_the_base`、`overhang_uses_the_neighbouring_kana_only`、`ruby_increases_the_line_advance_when_needed`、`line_without_ruby_is_unchanged`。
- [ ] 試験 `vertical_ruby_is_on_the_right`、`ruby_does_not_enter_the_selection`（`hit_test`、`navigate` の戻りと `selection_rects` が、ルビを足す前と同じ）。
- [ ] 試験（`model.rs`）`ruby_range_cannot_cross_a_newline`、`ruby_entry_count_must_match_the_base`、`deleting_the_base_removes_the_ruby`（`remap_range`）。
- [ ] 試験（`text_bake_tests.rs`）`ruby_bakes_after_the_body`、`bake_over_the_glyph_limit_is_refused`（`RESOURCE_LIMIT`、状態が変わらない）、`ruby_does_not_change_existing_bakes`。
- [ ] GPU 画素試験（`m7-tests.js`）`ruby renders at its bake position`、`GPU loss rebuilds ruby`。
- [ ] 完了条件：`git diff tests/parity` が新しい行の追加だけ。ルビ付きの文書が保存、ジャーナル、再オープン、コミット、差分で往復する。
- [ ] 形式の番号の更新（`alto-store` の `document_format` を含む）は共通の設計に従う。
- [ ] 検証：`cargo test -p alto-text`、`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。PR。

### 依存

- 先に：TXT-5（remap_range、段落ごとの行頭位置の整理）。
- 後に：TXT-2（行の余白の計算を共有する）、TXT-3（スタイルの出所の分離を使う）、VER-3（08。ルビの範囲を追従する）。

## TXT-2 圏点

### 現状

- 圏点の型も配置も無い。
- 字形の探索は `Itemizer::font_for(style, grapheme)`（`layout.rs`）が、フォールバックの順に `face.glyph_index` で調べ、無ければ `(first, missing)` を返す。欠けた字は `TextLayout.missing` と、フォントが無ければ `GlyphRef { font: None }` の箱になる。
- 欠けた字形の箱は `placeholder_outline`（`text.rs`）。円や記号の輪郭を合成する仕組みは無い。

### 11-04 からの変化

- 変化なし。旧設計の `EmphasisMark { sesame | dot | circle | triangle | custom(char) }` から、CSS の text-emphasis-style と同じ double_circle を足した。

### 設計

```rust
pub enum EmphasisMark { Dot, Circle, DoubleCircle, Triangle, Sesame, Custom(char) }
pub enum Side { Over, Under }       // 横組みは上と下、縦組みは右と左
pub struct EmphasisRange { pub start: u32, pub end: u32, pub mark: EmphasisMark, pub side: Side, pub filled: bool }
// TextContent.emphasis: Vec<EmphasisRange> — 始点順、重ならない、MAX_EMPHASIS 以下
```

- 範囲の追従は `RangePolicy::Style`。改行をまたげる（空白と改行には印を付けない）。
- 印の文字：Dot は • U+2022（白は ◦ U+25E6）、Circle は ● U+25CF（○ U+25CB）、DoubleCircle は ◉ U+25C9（◎ U+25CE）、Triangle は ▲ U+25B2（△ U+25B3）、Sesame は ﹅ U+FE45（﹆ U+FE46）、Custom(c) は c（書記素1つ、空白と制御文字は拒否）。
- 字形の出所：その範囲の文字のスタイルのフォールバックの順に `font_for` で探す。どれにも無ければ円の輪郭を合成する（`Custom` を含む）。合成する円は 1000 単位の箱の中心 (500, 500)、半径 400 の4本の3次ベジェ（制御点は半径の 0.5523 倍を整数に丸める）。白は半径 300 の逆向きの円を足してリングにする（`placeholder_outline` が穴を逆回りで作るのと同じ）。三角などもこの円で代用する。
- 大きさと位置：印の大きさは親の RUBY_SIZE_RATIO（TXT-1。0.5）倍。親字ごとに、その `ClusterBox` の中央に置き、ブロック方向は親の em の外側（`Over` は横組みで親の字形の上端、縦組みで右端）へ接する。縦組みの `Under` は左。
- 行の余白：印の高さがあるとき、ルビと同じ `extra` の計算に加える。同じ側にルビと圏点と傍線（TXT-6）があるときは、親字、傍線、圏点、ルビの順に外へ積む（ルビを圏点の高さだけ外へ寄せる）。
- 合成の輪郭：`GlyphRef { font: None, glyph }` は今、どの `glyph` でも `placeholder_outline` の箱になる（`bake_text` と `OutlineCache::outline` が `font: None` を箱へ写す）。これでは円が焼けないので、`glyph` の予約値 `SYNTH_CIRCLE`（0xFF00。塗り）、`SYNTH_RING`（0xFF01。白）、`SYNTH_RULE`（0xFF02。TXT-6 の箱）（新規）を決める。`glyph == 0` は従来の箱のまま。`bake_text` の輪郭の鍵 `(Option<AssetId>, u16)` は予約値ごとに別の鍵になり、`bake_text` と `OutlineCache` が予約値を合成の輪郭へ写す。フォントの実際の字形の番号は 0xFF00 以上でも `font: Some` なので衝突しない。
- 焼き込み：本文、ルビの後ろに焼く（共通の設計）。
- SDK：`doc.text.setEmphasis(id, start, end, spec | null)`。意図は `"text.emphasis"`。

### 形式と通信

- `format_version`：上げる（TXT-1 と同じ回。値は要割り当て）。
- `PROTOCOL`：上げない。ALTB と描画：変えない。`text-bake.txt` に `text-v3-emphasis`（新規）を足す。
- 差分：`ContentDetail::Text` に `emphasis: bool`（新規）。

### 作業手順

**Files:** TXT-1 と同じファイルに加えて Modify `crates/alto-text/src/outline.rs`（`OutlineCache` が合成の輪郭を返す。下の「合成の輪郭」）。

**Interfaces:** Consumes `remap_range`、TXT-1 の行の余白の計算。Produces `EmphasisRange`、`doc.text.setEmphasis`。

- [ ] 試験 `emphasis_mark_is_centered_on_each_base`、`vertical_emphasis_is_on_the_right`、`under_emphasis_is_on_the_other_side`、`emphasis_skips_spaces`。
- [ ] 試験 `emphasis_falls_back_to_a_drawn_circle`（フォントに記号の字形が無いとき、`font: None` の輪郭ではなく円のコマンドが焼かれる）、`emphasis_with_ruby_stacks_outside`。
- [ ] 試験 `emphasis_range_follows_edits`（`Style` の規則）。
- [ ] 形式の番号の更新（`alto-store` の `document_format` を含む）は共通の設計に従う。
- [ ] 完了条件・検証：TXT-1 と同じ。PR。

### 依存

- 先に：TXT-1（行の余白、焼き込みの順序）、TXT-5（remap_range）。
- 後に：なし。

## TXT-3 割注

### 現状

- 割注の型も配置も無い。
- 行分割は `layout` の貪欲な走査で、単位は `Cluster` の列。1つのクラスターは分割されず、行をまたがない。禁則は `shape_paragraph` が `allow_break` を偽にして表す。
- 選択の幾何は `LineBox` の `block`（行の厚み）と `ClusterBox.inline`（行に沿った範囲）だけで作る（`select.rs`）。1つの行に2段のテキストを持つ構造は無い。

### 11-04 からの変化

- 変化なし。旧設計は「`layout.rs` の行分割を『インラインボックス』の列へ整理してから入れる」と書いた。この方針を残し、整理を単独の PR にする手順を具体化した。

### 設計

```rust
// TextContent.warichu: Vec<TextRange> — 始点順、重ならない、1段落内、MAX_WARICHU 以下、1組 MAX_WARICHU_UTF16 以下
```

- 範囲の追従は `RangePolicy::Style`。ルビの親と重ならない。入れ子は拒否。
- 割注の中身は本文の一部（`content.text` の範囲）で、親の `size × 0.5` で整形する。

**行分割の整理（TXT-3a、単独の PR）。** `layout` の `Vec<Cluster>` を `Vec<Inline>` にする（`Inline::Cluster(Cluster)` だけ）。貪欲な走査を `Inline` の幅と `allow_break` で書き直す。出力は1バイトも変えない（`tests/parity/text-bake.txt` と `crates/alto-text/tests/layout.rs` が不変）。

**割注（TXT-3b）。**

1. `Inline::Warichu(WarichuBox)`（新規）が、割注の中身の `Cluster` の列を持つ。
2. 行分割は、現在の行の残り幅 `R` に対し、箱を `split(R)` で分ける。2行分の容量 `2R` に収まる最大の前方のクラスター列（行分割の機会で切る）を取り、それを幅がなるべく等しい2つの半分に分ける（上の半分が下より長いか等しい）。残りは次の行の先頭から新しい箱として続ける。
3. 箱が占める幅は長いほうの半分の幅、厚みは親の 1 em。2つの半分は親の em の前半と後半に置く（横組みは上の段が先、縦組みは右の段が先。どちらの段も通常の進行方向に読む）。ベースラインは親の中心線に対する ±`0.25 × size` の位置。
4. 割注の前後に括弧を付けるかは本文の文字で決める（エンジンは括弧を足さない）。
5. 選択の幾何：`ClusterBox` に `block: Option<[f64; 2]>`（新規）（段の厚み）を足し、割注のクラスターだけが値を持つ。`select.rs` は値があるとき、行全体の厚みの代わりにその範囲で矩形を作る。ヒットテストは `block` の近い段を選ぶ。

行の余白：割注は 1 em の厚みに収まるので、行の太さを変えない。

SDK：`doc.text.setWarichu(id, start, end, on = true)`。意図は `"text.warichu"`。

### 形式と通信

- `format_version`：上げる（TXT-3 の回。値は要割り当て）。TXT-3a は形式を変えない。
- `PROTOCOL`：上げない。ALTB と描画：変えない。`text-bake.txt` に `text-v3-warichu`（新規）。
- 差分：`ContentDetail::Text` に `warichu: bool`（新規）。

### 作業手順

**Files:** Modify `crates/alto-text/src/layout.rs`、`model.rs`、`select.rs`、`crates/alto-engine/src/text.rs`、`crates/alto-doc/src/version.rs`、`crates/alto-git/src/lib.rs`、`packages/alto-sdk/src/types.ts`、`document.ts`、`text-edit.ts`、`intents.ts`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`。

**Interfaces:** Produces `Inline`、`WarichuBox`、`doc.text.setWarichu`。

- [ ] TXT-3a：インラインボックス化を単独の PR で行い、`text-bake.txt` と既存の `layout.rs` 試験が不変であること。
- [ ] TXT-3b：試験 `warichu_splits_into_two_lines`（半分の幅の差が 1 クラスター以内）、`warichu_breaks_across_lines`（行の残りが小さいとき、前半が前の行に入り、残りが次の行で2段のまま続く）、`warichu_selection_rects_follow_the_half_lines`、`warichu_does_not_change_other_text`。
- [ ] 形式の番号の更新（`alto-store` の `document_format` を含む）は共通の設計に従う。
- [ ] 完了条件・検証：TXT-1 と同じ。段階3の項目で、先に TXT-1、TXT-2 を入れ終えていること。

### 依存

- 先に：TXT-1（スタイルの出所の分離）、TXT-5（remap_range）。
- 後に：なし。

## TXT-4 ぶら下げ

### 現状

- 行頭禁則は `no_line_start` が `breaks[i] = false` にして、句読点と直前の文字を一緒に次の行へ追い出す（`shape_paragraph`）。ぶら下げは無い。
- 行の長さの判定は `layout` の `width + c.advance > avail + 1e-9`。`measured`（行末の空白を除く送りの合計）と `extra`（`Justify` の字間）が揃えに使われる。

### 11-04 からの変化

- 変化なし。

### 設計

`TextContent.hanging: bool`（既定 `false`、`skip_serializing_if`）。`true` のとき、`、`、`。`、`，`、`．` の `Cluster` を 1 つだけ、行末に残してよい。

- 判定（`layout` の貪欲な走査）：クラスター `c` がこの4字で、そこまでの `width <= avail` のとき、`width + c.advance > avail` でも行を切らない。ぶら下げた字の送りは `width` に足さず、その字の直後で行を終える（`end = i + 1`。足さないだけでは次のクラスターが同じ行に入ってしまうので、終わりを明示する）。判定は今の `i > start && !c.space` の条件の内側で行い、行頭の字と空白は対象にしない。連続する句読点の2つ目は禁則で行頭に来られないので、2つ目が溢れると今と同じ追い出しになる（ぶら下げは1字まで）。
- 揃え：ぶら下げた字は `measured` から除く。`Justify` の `gaps` と `extra` もぶら下げた字を除いて計算する。`End`、`Center` では、ぶら下げた字を除いた文字列を枠に揃え、ぶら下げた字は枠の外へ出る。
- 縦組みでも同じ（行の長さは高さ）。
- `LineBox.inline` と `TextLayout.bounds` はぶら下げた字を含める（描画される範囲）。枠（`TextFrame::Area`）の外へ出るぶん、`bounds` が枠より広がる。
- 点テキスト（`avail` が無限）では何も変わらない。

### 形式と通信

- `format_version`：上げる（TXT-4、5、6、7 で1回。値は要割り当て）。
- `PROTOCOL`：上げない。ALTB と描画：変えない。`text-bake.txt` に `text-v3-hanging`（新規）。
- 差分：`ContentDetail::Text` に `hanging: bool`（新規）。

### 作業手順

**Files:** Modify `crates/alto-text/src/model.rs`、`layout.rs`、`crates/alto-engine/src/text.rs`、`crates/alto-doc/src/version.rs`、`crates/alto-git/src/lib.rs`、`packages/alto-sdk/src/types.ts`、`document.ts`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`、`packages/alto-sdk/api/alto-engine.api.md`。

**Interfaces:** `TextContent.hanging`、`doc.text.setHanging(id, on)`（新規）。意図は `"text.edit"`。

- [ ] 試験 `hanging_keeps_the_period_on_the_line`、`hanging_does_not_count_in_justify`、`hanging_stops_at_one_character`、`vertical_hanging_uses_the_height`、`hanging_off_is_unchanged`。
- [ ] 試験 `new_text_fields_serialize_as_before`（共通）。
- [ ] 既存の焼き込み行が不変であることを確かめ、PR。検証は共通の3コマンド。

### 依存

- 先に：なし（段階1）。
- 後に：TXT-5（同じ `layout` の行の幅を変えるので、順序は TXT-4 が先）。

## TXT-5 字下げ、段落ごとの揃え

### 現状

- 揃えは `TextContent.align` の1つだけ。`layout` が `content.align` を使い、最終行と点テキストの `Justify` を `Start` にし、右から左の段落で `Start` と `End` を入れ替える。
- 段落は `layout` が `text.split('\n')` で区切る単位で、`LineBox.paragraph` に番号がある。
- 折り返し幅は `avail` 1つで、行頭の位置は 0（揃えで動く）。

### 11-04 からの変化

- 変化なし。旧設計は `ParagraphRange { start, end, ... }` の範囲だった。空の段落（`a\n\nb`）の始点と終点が隣と区別できず、改行の編集で曖昧になる。段落の添字で持つ形に変えた。

### 設計

```rust
pub struct ParagraphAttr {
    pub index: u32,                       // 0 始まりの段落番号（改行の数え方）
    #[serde(default)] pub align: Option<TextAlign>,
    #[serde(default)] pub indent_first: f32,   // 1行目の追加字下げ（em。負で先頭だけ張り出す）
    #[serde(default)] pub indent_start: f32,   // 行頭側の字下げ（em）
    #[serde(default)] pub indent_end: f32,     // 行末側の字下げ（em）
}
// TextContent.paragraphs: Vec<ParagraphAttr> — index の昇順、MAX_PARAGRAPH_ATTRS 以下、すべて既定値の項目は持たない
```

- em は `content.style.size`（スタイルの基準サイズ）。範囲は各 −20〜20。`indent_start + indent_first >= 0`。`index` は段落数未満。
- 折り返し幅は avail から indent_start と indent_end を引いた値、1行目はさらに indent_first を引く値。揃えは、この幅の中で行う（Center はその中央）。行頭の位置は indent_start（1行目は indent_first を加える）。右から左の段落では行頭側が右になるので、左の余白が indent_end になる。点テキストでは indent_end を無視する。`align` が `None` の段落は `TextContent.align` を使う。
- 編集時の追従：remap_paragraphs（新規。`model.rs` と `text-edit.ts`）。編集 [start, end) を置き換えるとき、k を start の前の改行数とし、削除範囲の改行数を R、挿入文字列の改行数を A とする。段落 k から k + R までが1つに合わさり、先頭の k の属性が残る。そこから A + 1 個の段落に分かれ（改行 A 個で A + 1 段落）、新しい段落は k の属性を複製する。それ以降の添字は A − R だけずれる。削除で段落が減ったときは、k + 1 から k + R の属性を捨て、さらに新しい段落数以上の添字の属性を残さない（`replace_range` が最後に検証するので、範囲外の添字は拒否になる。span と縦中横を刈るのと同じ）。
- SDK：`doc.text.setParagraph(id, start, end, patch)`（新規）。`[start, end]` と重なるすべての段落（`end` が段落の途中でもその段落を含む）に `patch` を適用する。`patch` は `{ align?, indent_first?, indent_start?, indent_end? }` で、値 `null` で既定へ戻す。意図は `"text.paragraph"`。`doc.text.replace` は `remap_paragraphs` を通す。`doc.text.set` と `doc.text.setParagraph` は1回の `setText` で、Undo 1回に対応する。

### 形式と通信

- `format_version`：上げる（TXT-4、5、6、7 の回。値は要割り当て）。
- `PROTOCOL`：上げない。ALTB と描画：変えない。`text-bake.txt` に `text-v3-paragraph`（新規）。
- 差分：`ContentDetail::Text` に `paragraphs: bool`（新規）。

### 作業手順

**Files:** Modify `crates/alto-text/src/model.rs`（`remap_range`、`remap_paragraphs`、`replace_range` を共通関数で書き直す）、`layout.rs`、`crates/alto-doc/src/version.rs`、`crates/alto-git/src/lib.rs`、`packages/alto-sdk/src/types.ts`、`document.ts`、`text-edit.ts`、`intents.ts`、`packages/alto-sdk/test/`（`text-edit.test.js` を新規）、`tests/fixtures/text-remap.json`（新規）、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`、`examples/browser-playground/text-ui.js`。

**Interfaces:** Produces `ParagraphAttr`、`remap_range`、`remap_paragraphs`、`doc.text.setParagraph`。

- [ ] 試験（`layout.rs`）`paragraph_align_overrides_the_default`、`first_line_indent_shortens_the_first_line`、`indent_end_shortens_every_line`、`negative_first_indent_hangs_the_first_line`、`rtl_paragraph_swaps_the_indent_sides`。
- [ ] 試験（`model.rs`）`editing_text_keeps_paragraph_attributes`、`deleting_a_newline_merges_into_the_first_paragraph`、`splitting_a_paragraph_copies_the_attributes`、`remap_range_matches_the_old_span_rules`。
- [ ] 試験（SDK）`replaceText` と Rust の remap_range が、`tests/fixtures/text-remap.json`（新規）の全行で同じ結果を返す。
- [ ] 試験（`m7-tests.js`）`set_text_is_one_undo`（`setParagraph` の後の Undo 1回で戻る）、保存の往復。
- [ ] 形式の番号を上げる回では、`crates/alto-store/src/lib.rs` の `document_format` の書き出しと受け入れ、`DocumentMeta` の `format_version` を同じ PR で更新する（共通の設計）。
- [ ] 完了条件・検証：TXT-4 と同じ。PR。

### 依存

- 先に：TXT-4（行の幅の判定が同じ箇所）。
- 後に：TXT-1、TXT-2、TXT-3（remap_range）、VER-3（08。remap_range を使う）。

## TXT-6 下線

### 現状

- 下線は無い。字形以外の矩形を焼く手段は無い（`TextBake` は輪郭への参照を持つ字形の列だけ）。
- フォントの下線の値を読む処理は無い。`FontData`（`crates/alto-text/src/font.rs`）は `upem`、`ascent`、`descent`、`has_vert` だけを持つ。ttf-parser 0.25.1 の Face::underline_metrics は `post` テーブルから位置と太さを返し、可変フォントの `undo`、`unds` を反映する。`post` が無ければ `None`。
- 焼き込みの字形の配置は `m`（Q24）と `t`（Q30）で、輪郭は Q6、座標の絶対値は `MAX_OUTLINE_COORD`（2^22）以下。

### 11-04 からの変化

- 変化なし。旧設計の「矩形レコード」は焼き込みの形式を変えるため、既存の字形の仕組みで表す形に直した（下記）。

### 設計

```rust
pub enum UnderlineKind { None, Solid }
pub struct Underline {
    pub kind: UnderlineKind,            // 既定 None。StylePatch では None が「下線を消す」
    #[serde(default)] pub color: Option<Color>,        // None は文字色
    #[serde(default)] pub position: Option<f32>,       // em。下向き正。None はフォントの値
    #[serde(default)] pub thickness: Option<f32>,      // em。None はフォントの値
}
// TextStyle.underline: Underline（既定 None で省略）、StylePatch.underline: Option<Underline>
```

- 検証：`position` は −1〜1、`thickness` は 0.005〜0.5、色は 0〜1。`patched` と `validate` が扱う。
- 値の決め方：`position` と `thickness` が `None` なら、そのクラスターのフォントの Face::underline_metrics（ttf-parser）を size / upem 倍する（position は post の underlinePosition。上向き正なので符号を反転して下向き正にする）。`post` が無いフォントは `UNDERLINE_POSITION_EM`（新規）= 0.12 と `UNDERLINE_THICKNESS_EM`（新規）= 0.05。
- 配置（`layout`）：行ごとに、同じ `Underline` と同じ `size` が連続するクラスターを1本の線にまとめる（行末の空白を除く）。`TextLayout.rules: Vec<PlacedRule>`（新規）（`rect: [f64; 4]` ノード座標、`color`）に入れる。横組み：線の中心は `baseline + position`、長さはクラスターの `inline` の端から端まで。縦組みは右側の傍線で、`x = baseline + size / 2 + VERTICAL_RULE_GAP_EM（新規）（0.1 em） × size`、`y` は同じく `inline` の範囲。ルビが同じ側にあるとき（縦組み）、ルビの内端を傍線の外縁まで寄せる。
- 焼き込み：矩形は輪郭 1 個（1000 単位の箱。`Move`、`Line` ×3、`Close`。`font` は `PLACEHOLDER_FONT`、`glyph` は 0）を共有し、各線を `m = [w / 1000, 0, 0, -h / 1000]`（Q24）、`t` = 左上、色 Q14 の `BakeGlyph` として足す。`bake_text` の輪郭の重複除去の鍵は `(Option<AssetId>, u16)` で、箱の `(None, 0)` と衝突するので、TXT-2 の「合成の輪郭」の予約値 SYNTH_RULE を使う。線の描画はアトラスか直接ラスタ化かを字形と同じ判定で選ぶ。描画の3実装は変わらない。
- 下線を持たない文書の `rules` は空で、焼き込みは変わらない。
- SDK：`StylePatch.underline`（`types.ts` に `Underline` を足す）。`doc.text.style(id, start, end, { underline })` を使う。新しい関数は足さない。

### 形式と通信

- `format_version`：上げる（TXT-4、5、6、7 の回。値は要割り当て）。`TextStyle` と `StylePatch` に項目が加わる。
- `PROTOCOL`：上げない。ALTB と描画：変えない。`text-bake.txt` に `text-v3-underline`、`text-v3-underline-vertical`（新規）。
- 差分：`ContentDetail::Text.style` は `x.style != y.style || x.spans != y.spans` なので追加は不要。

### 作業手順

**Files:** Modify `crates/alto-text/src/model.rs`、`font.rs`（下線の値を `FontData` に持たせる）、`layout.rs`、`crates/alto-engine/src/text.rs`、`packages/alto-sdk/src/types.ts`、`document.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`。

**Interfaces:** Produces `Underline`、`PlacedRule`、`TextLayout.rules`。

- [ ] 試験 `underline_uses_the_font_metrics`（`post` の値と一致）、`underline_falls_back_without_a_post_table`、`vertical_underline_is_on_the_right`、`underline_splits_at_a_size_change`、`underline_skips_trailing_spaces`、`rule_bakes_one_shared_outline`。
- [ ] 試験 `no_underline_bakes_the_same_bytes`（追加前の3つの parity テキスト）。
- [ ] 形式の番号を上げる回では、`crates/alto-store/src/lib.rs` の `document_format` の書き出しと受け入れ、`DocumentMeta` の `format_version` を同じ PR で更新する（共通の設計）。
- [ ] 完了条件・検証：TXT-4 と同じ。PR。

### 依存

- 先に：なし（段階1）。縦組みの傍線とルビの寄せは TXT-1 の後で確かめる。
- 後に：TXT-2（積み順）。

## TXT-7 斜体の合成

### 現状

- 斜体の合成は無い。字形の変換は `place`（`layout.rs`）が作る `transform: [f64; 6]`（フォント単位の y 上向きからノードの y 下向きへの行列）で、焼き込みでは `m` の4成分を Q24 にする。
- 行列は向きごとに違う：横組みは `[s·k, 0, 0, -s, ...]`、縦組みの直立は同じ形で原点だけ違い、横倒しは `[0, s·k, s, 0, ...]`、縦中横は横組みと同じ形。

### 11-04 からの変化

- 変化なし。旧設計の「焼き込みの字形変換 `t: [f64; 6]`」は誤りで、焼き込みは `m: [i32; 4]`（Q24）と `t: [i64; 2]`（Q30）。内容は `place` の `transform` を Q 値にしたもの。

### 設計

`TextStyle.italic_skew: f32`（度。−30〜30、既定 0。`skip_serializing_if` で 0 は省略）、`StylePatch.italic_skew: Option<f32>`。

- 傾き `t = tan(italic_skew)`。`place` が `transform` を作った後、向きで次のように足す。`transform = [a, b, c, d, e, f]`（列優先）。
  - 横組み、横倒し（`Sideways`）、縦中横：フォント空間のせん断 `x' = x + y·t` なので `c += t·a`、`d += t·b`。
  - 縦組みの直立（`Upright`）：進行方向（縦）に沿ったせん断なので、列の中心線 `baseline`（`place` の引数）を軸に `y' = y + (x − baseline)·t`：`b += t·a`、`d += t·c`、`f += t·(e − baseline)`。
- 検証：有限、−30〜30。0 のときは何もしない（焼き込みが今と同じバイト）。
- 決定性：`f64::tan` は libm 依存で、最後の桁がネイティブと WASM で違う可能性がある。Q24 に丸めるので差が出る確率は極めて低いが、`text-v3-skew`（新規）の行で両方の一致を確かめる。判定条件：ネイティブと WASM でバイトが違ったら、角度を 0.25° 刻みに丸め、`tan` を整数表（`crates/alto-text` にコミットする生成物）に置き換える。
- 実フォントの斜体（イタリック）との選択は利用者がフォントで行う。合成はフォントが無いときの代用。
- SDK：`doc.text.style(id, start, end, { italic_skew })`。

### 形式と通信

- `format_version`：上げる（TXT-4、5、6、7 の回。値は要割り当て）。
- `PROTOCOL`：上げない。ALTB と描画：変えない（行列に入るだけ）。`text-bake.txt` に `text-v3-skew`、`text-v3-skew-vertical`（新規）。

### 作業手順

**Files:** Modify `crates/alto-text/src/model.rs`、`layout.rs`、`packages/alto-sdk/src/types.ts`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`、`packages/alto-sdk/api/alto-engine.api.md`。

**Interfaces:** Produces `TextStyle.italic_skew`。

- [ ] 試験 `skew_zero_bakes_the_same_bytes`、`horizontal_skew_moves_the_top_right`（y の大きい点ほど x が増える）、`vertical_upright_skew_shears_along_the_column`、`skew_out_of_range_is_refused`、`skew_applies_to_the_span_only`。
- [ ] 完了条件・検証：TXT-4 と同じ。ネイティブと WASM（ブラウザ）の `text-v3-skew` が一致。PR。

### 依存

- 先に：なし（段階1）。
- 後に：なし。

## TXT-8 可変フォント

### 現状

- 可変フォントの軸を設定する処理は無い。set_variation を呼ぶ箇所は無い（`doc/design/2026-10-28-v3-integer-text.md` の「範囲」も対象外としている）。
- 依存：rustybuzz 0.20.1 の Face::set_variations（引数は Variation { tag, value: f32 } の列）が使える。ttf-parser 0.25.1 は variable-fonts 機能が有効で、variation_axes（fvar）、set_variation（タグと値）、outline_glyph（gvar と CFF2 の補間）を持つ。ただし ttf-parser の座標は最大 64 軸。
- 顔は `layout` の `Itemizer.faces: HashMap<AssetId, rustybuzz::Face>`（フォント1つにつき1つ）と、`raw_outline`（`crates/alto-text/src/outline.rs`）の `ttf_parser::Face::parse`、`OutlineCache` の `(AssetId, u16)` が作る。
- v3 の描画は焼き込みの輪郭だけから字形を作る。`glyph_q8::atlas_key` は輪郭のバイト列の blake3 だが試験でしか使われず、描画（`paint.rs`）は切り出しごとの仮の鍵で詰める。軸の値は補間後の輪郭に現れるので、描画側に軸の知識は要らない。`BakeFont` は描画が読まない。
- 輪郭の重複除去は `bake_text` の `outline_index: HashMap<(Option<AssetId>, u16), u32>`。

### 11-04 からの変化

- 旧設計は「字形アトラスの鍵に軸値（Q16）を追加」「`BakeFont` に同じ値を載せる」と書いた。v3 の描画は補間後の輪郭だけを読み、軸の知識を持たない（`atlas_key` は輪郭の内容から作るが、試験でしか使われない）。`BakeFont` は描画が読まない。どちらも変更しない。変えるのは `bake_text` の重複除去の鍵と `OutlineCache` の鍵（情報用の経路）。
- 旧設計の `FontVariation { axis: [u8; 4], ... }` は `FontFeature` と同じ `tag: String`（4文字の ASCII）に改めた。

### 設計

```rust
pub struct FontVariation { pub tag: String, pub value: f32 }
// TextStyle.variations: Vec<FontVariation>、StylePatch.variations: Option<Vec<FontVariation>>、MAX_VARIATIONS 以下
```

- 検証（`model.rs`）：タグは4文字の ASCII（`FontFeature` と同じ）、重複しない、値は有限。
- 検証（エンジン）：軸の範囲は `fvar` を読まないと分からない。`alto-doc` はフォントのバイトを持たないので、`Engine::bake_ops` の前に `Engine::check_variations`（新規）で調べる。スタイルのフォールバックの各フォントについて、`fvar` がそのタグを持つなら `min..=max` の外を拒否する（clamp しない。`INVALID_ARGUMENT: variation axis 'wght' = 900 is outside 100..=700`）。持たないフォントでは無視する。どのフォントにも無い軸は拒否する（綴りの誤りを防ぐ）。読めないフォントは検査から外す。
- 整形（`layout.rs`）：顔の鍵を `(AssetId, VarId)` にする。`VarId` はそのレイアウトで使う軸の組の通し番号（先頭に使われた順、0 は軸なし）。値は `round(value × 65536)` の Q16 に丸めてから f32 に戻して `Face::set_variations` に渡す（同じ設定が同じ顔になる）。`GlyphRef` に `var: u16`（新規）（`TextLayout.variations` の添字）を足す。
- 輪郭（`outline.rs`）：`raw_outline(font, glyph, variations)` が `ttf_parser::Face` に set_variation を適用してから `outline_glyph` を呼ぶ。`OutlineCache` の鍵に Q16 の組を足す。
- 焼き込み：`outline_index` の鍵に VarId を足す。軸なしのとき（VarId が 0）は今と同じ並びとバイト。
- 行の高さ（`ascent`、`descent`）は既定の位置の値を使い、`MVAR` は反映しない。
- SDK：`FontInfo.axes?: { tag, min, default, max }[]`（`importFont` の戻りに追加。`inspect` が `fvar` を読む）、既にある文書のフォント用に `engine.fonts.axes(asset)`（新規）（`fontAxes` 要求）。`doc.text.style(id, start, end, { variations })` を使う。
- テスト用の小さな可変フォントを `tests/fonts` に OFL で足す（Noto Sans の可変版のサブセット、1軸）。`THIRD-PARTY-NOTICES.md` と OFL の本文を更新する。
- 依存の版：`text_bake_dependencies_are_pinned` が固定している `rustybuzz`、`ttf-parser` のロック版を変えない。

### 形式と通信

- `format_version`：上げる（TXT-8 で1回。値は要割り当て）。
- `PROTOCOL`：上げる。新しい要求 fontAxes（値は要割り当て）。README と RELEASING を同じ PR で更新する（共通規則3）。
- ALTB と描画：変えない。`text-bake.txt` に `text-v3-variable`（新規）。
- 差分：`ContentDetail::Text.style` が拾う。

### 作業手順

**Files:** Modify `crates/alto-text/src/model.rs`、`font.rs`、`layout.rs`、`outline.rs`、`crates/alto-engine/src/text.rs`、`crates/alto-wasm/src/text.rs`、`packages/alto-sdk/src/protocol.ts`、`worker.ts`、`engine.ts`、`types.ts`、`document.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`alto-engine-browser.api.md`、`README.md`、`RELEASING.md`、`THIRD-PARTY-NOTICES.md`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`。Create `tests/fonts/AltoTestVariable.ttf`（新規）、`tests/fonts/OFL-NotoSansVariable.txt`（新規）。

**Interfaces:** Produces `FontVariation`、`FontInfo.axes`、`engine.fonts.axes`。

- [ ] 試験 `variation_changes_the_outline`（軸の値で焼いた輪郭が違う）、`axis_out_of_range_is_refused`（状態が変わらない）、`unknown_axis_is_refused`、`axis_missing_in_one_fallback_font_is_ignored`、`no_variation_bakes_the_same_bytes`。
- [ ] 試験 `variation_bakes_a_different_outline_record`（`text_bake::outline_record` のバイト列が軸の値で変わる。実際に `layout` と `bake_text` が軸を適用したことを確かめる）、`same_axes_share_an_outline`（2つの span が同じ軸値なら輪郭を共有）。
- [ ] 試験 `variation_advances_follow_the_axis`（`HVAR`/`gvar` の送りが整形に反映される）。
- [ ] 完了条件・検証：TXT-4 と同じ。ネイティブと WASM で `text-v3-variable` が一致。PR。

### 依存

- 先に：なし（段階2）。
- 後に：TXT-9（顔の鍵 `(AssetId, VarId)` を共有する）、TXT-10。

## TXT-9 カラーフォント

### 現状

- カラーフォントは対象外で、単色の `outline_glyph` だけを使う（`raw_outline`）。ttf-parser 0.25.1 は Face::is_color_glyph、color_palettes、paint_color_glyph（字形、パレット、前景色、Painter。COLR v0 と v1）、glyph_raster_image（sbix、CBDT など）を持つ。
- 色層のベイクは `BakeGlyph.color`（字形ごとの Q14 の色）で既に可能。同じ位置に色の違う字形を複数並べればよい。
- 輪郭が空の字形（ビットマップだけのフォントの字）は `bake_placed` が偽にして飛ばす。警告は `Engine::font_warnings`（`FONT_EMBEDDING_RESTRICTED`、`FONT_MISSING`）が出すが、これは `doc.assets` のフォントの属性と blob の有無だけを見る関数で、字形やレイアウトを入力に持たない。

### 11-04 からの変化

- 変化なし。旧設計の「ラスターのタイル化」は項目から外し、ビットマップだけの字形は警告と代替の箱にする。

### 設計

- 第一段階は COLR v0 と、v1 のうち v0 と同じ表現ができるもの（単色塗りの層だけ）。グラデーション、変換、合成モードを使う v1 の字形は単色の輪郭に戻し、警告 `COLOR_FONT_UNSUPPORTED`（新規）（`family` を持つ）を出す。COLR v1 の完全な対応はグラデーションの塗り（VEC-1、07）の後。
- `TextStyle.palette: u16`（既定 0。`skip_serializing_if`）。範囲外のパレットは 0。
- 焼き込み（`bake_text`）：字形が `is_color_glyph` のとき、`Face::paint_color_glyph` に `Painter` を渡す。`Painter` は `outline_glyph` + `push_clip` + 単色の `paint` + `pop_clip` の並びだけを層として受け取り、それ以外の呼び出しを見たら全体を単色に戻す。層ごとに `BakeGlyph`（層の字形の輪郭、親の `m`、`t`、色 = 層の色 × 文字色のアルファ。パレットの前景色（0xFFFF）は文字色）を層の順に足す。1字形あたり最大 `MAX_COLOR_LAYERS`（新規）（64）、合計は `MAX_BAKE_GLYPHS`。
- 新しいシェーダーは要らない：単色の字形の経路を色ごとに通すのと同じ。`TextInfo.glyphs` は層を数えない（レイアウトの字形数のまま）。
- ビットマップの字形（`sbix`、`CBDT`、`EBDT`、`bdat`）：表示サイズに依存するので扱わない。`glyph_raster_image` が値を返す字形で輪郭が空のものは、`placeholder_outline` の箱を焼く。警告 `FONT_COLOR_BITMAP_ONLY`（新規。`family` を持つ）は、`font_warnings` に節を足して出す：文書の各テキストノードの `text_entry` のレイアウトで使われた字形（`glyphs`）のうち、輪郭が空で `glyph_raster_image` が値を返すものがあるフォントを、フォントごとに1回数える（面の顔は `Engine.text.fonts` のキャッシュ）。`COLOR_FONT_UNSUPPORTED`（新規）も同じ節で、色層の字形が受理できない塗りを含むフォントを数える。警告は計算で出すだけで、文書には保存しない。
- 可変軸（TXT-8）と合わせるとき、色層の字形にも同じ軸を適用する。

### 形式と通信

- `format_version`：上げる（`palette` を足すとき。TXT-8 と同じ回にしてよい。値は要割り当て）。
- `PROTOCOL`：上げない（警告は既存の `warnings` に載る）。ALTB と描画：変えない。`text-bake.txt` に `text-v3-colr`（新規）。依存の版を変えるとき以外、`BAKER_VERSION` は上げない。
- 試験用の COLR v0 フォントを `tests/fonts` に OFL で足す。

### 作業手順

**Files:** Modify `crates/alto-text/src/model.rs`、`font.rs`、`outline.rs`、`crates/alto-engine/src/text.rs`、`packages/alto-sdk/src/types.ts`、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`、`THIRD-PARTY-NOTICES.md`。Create `tests/fonts/AltoTestColor.ttf`（新規）。

**Interfaces:** Produces `TextStyle.palette`、警告 `COLOR_FONT_UNSUPPORTED`、`FONT_COLOR_BITMAP_ONLY`。

- [ ] 試験 `colr_v0_draws_layers_in_order`、`colr_layer_alpha_multiplies_the_text_alpha`、`colr_foreground_uses_the_text_color`、`colr_v1_gradient_falls_back_with_a_warning`、`sbix_only_font_warns_and_draws_the_fallback_box`、`color_layers_count_toward_the_bake_limit`。
- [ ] 形式の番号を上げる回では、`crates/alto-store/src/lib.rs` の `document_format` の書き出しと受け入れ、`DocumentMeta` の `format_version` を同じ PR で更新する（共通の設計）。
- [ ] 完了条件・検証：TXT-4 と同じ。PR。

### 依存

- 先に：TXT-8（顔の鍵）、TXT-1（焼き込みの字形数の上限）。
- 後に：なし。COLR v1 の完全対応は VEC-1 の後。

## TXT-10 ヒンティング、ステム強調、ガンマ、LCD

### 現状

- 文字のカバレッジは CPU の整数ラスタライザ（`crates/alto-gpu/src/glyph_q8.rs`、`raster_q8.rs`）が作り、`shaders/glyph-q14.wgsl` が読んで着色する。ヒンティング、ステム強調、ガンマ補正、LCD はどれも無い。`doc/design/2026-10-28-v3-integer-text.md` の「ヒンティングと AA のリスク」と未決事項4が将来の候補として残している。
- `glyph_map_q(world, level, m, t)` が焼き込みの `m`、`t` にノードの変換とレベルを合成する。ベースラインのノード座標は焼き込みには無く、ノードの変換とレベルが決まって初めて出力画素の位置が分かる。
- 文書の描画属性は `DocumentMeta`（`crates/alto-doc/src/lib.rs`）に `width`、`height`、`color`、`depth`、`format_version`、`render_version`。`render_version` は現在 3 だけが有効。

### 11-04 からの変化

- 旧設計は「ベースラインと x-height を出力画素に揃える整数の変換を、焼き込みの字形座標に適用する」と書いた。焼き込みはズームとノードの変換に依らないので、ここでは画素の位置が分からない。ヒンティングは描画側の `glyph_map_q` の後で行う形に直した。
- 旧設計の text_rendering の coverage_gamma（f32）は、`powf` の結果が環境で変わりうるので、固定の表から選ぶ形にした。

### 設計

render v4 のみ（RV4-1 の上。`render_version` が 4 の文書だけが持てる）。文書属性 `DocumentMeta.text_rendering: Option<TextRendering>`（新規）（RV4-1 の `render_version` と同じ形式の回）。

```rust
pub struct TextRendering {
    pub hint_y: bool,           // 既定 false
    pub stem_darken: u8,        // 0..=64。1/64 px 単位の強調の量。0 は無し
    pub coverage_gamma: u8,     // 80, 100, 120, 140, 180, 220 のいずれか（百分の一）。100 は恒等
}
```

- **ヒンティング。** `glyph_map_q` の後、字形の行列が軸に平行（`b = c = 0`、`a > 0`、`d < 0`。横組みの水平の字形）のとき、原点の y（`t_y`）をレベルの整数画素に丸める。TrueType の命令は実行しない。x 方向と送りは変えず、組版の位置は変えない。x-height の揃えは、案として `d` を x-height が整数画素になる倍率に補正する（x-height はフォントの `OS/2` から焼き込み時でなく描画時に必要なので、`BakeFont` の情報では足りない。判定条件：ベースラインの丸めだけで画質が足りるかを測ってから x-height を足すか決める。既定は丸めだけ）。
- **ステム強調。** 表示サイズ（em）が 24 px 未満の字形だけ。輪郭の点を、両隣の辺の法線の和の方向へ最大 `stem_darken / 64` px（ミターの上限は 2 倍）動かす。整数の計算（`isqrt`）で、Q8 の点に対して行う。ラスタ化（`glyph_q8`）で適用する。ただし `glyph_bitmaps(bake, world, level, roi)` は焼き込み、変換、レベル、ROI だけを受け取り、v3 と v4 が同じ焼き込みと同じ関数を共有する（01 の RV4-1）。文書の text_rendering（新規）はここに届かず、そのまま入れると v3 の画素も変わる。v4 専用の引数 `TextRenderParams`（新規。hint_y、stem_darken、coverage_gamma。v3 は None）を `glyph_bitmaps` と `glyph_map_q` に通し、`paint.rs` が文書の `render_version` と text_rendering から作る。`None` のとき処理は今と同じ。ヒンティングとガンマも同じ引数で渡す。
- **ガンマ補正。** カバレッジのバイトを、固定の 256 点の整数表で変換してから合成する。表は `crates/alto-gpu/src/glyph_gamma.rs`（新規）に生成物としてコミットする。指定できるのは上記6つだけ（他は `INVALID_ARGUMENT`）。
- **LCD。** 下地が不透明でないと成立せず、編集可能な合成と両立しない。表示経路の専用処理（文書のハッシュの対象外）とし、ノードの直下が不透明と判定できる場合だけ、提示の段で3倍の横カバレッジを使う。表示タイルのキャッシュを別に持つため、最後に実装し、測定で高価なら既定を無効にする。書き出しでは使わない。
- 3実装：Rust の参照（`crates/alto-gpu/src/cpu_composite/` の文字の参照）、JS の参照（`tests/e2e/cpu-composite.js`）、WGSL（01 が RV4-1 で作る `glyph-q30.wgsl`（新規））。Rust の参照は試験専用（`cpu_composite/text_tests.rs`）で、本番の経路は `renderer/tile_renderer/paint.rs` と `glyph_q8`。ガンマ表は3つで同じ表を使う。

### 形式と通信

- `format_version`：上げる（文書属性を足す。RV4-1 と同じ回。値は要割り当て）。
- `render_version` は 4（RV4-1 が定義）。`PROTOCOL`：上げる（文書の作成や変換の引数にこの属性を足すとき。値は要割り当て）。
- シェーダーのモード番号：文字のシェーダーは別ファイルなので、効果のモード番号（34 から）は使わない。必要になれば `glyph-q30.wgsl`（新規。01 に合わせた名前）に独自の番号を持つ（要割り当て）。
- v3 の文書は、ヒンティングなどの有無にかかわらず今のハッシュを保つ（試験は作業手順）。

### 作業手順

**Files:** Modify `crates/alto-doc/src/lib.rs`、`crates/alto-gpu/src/glyph_q8.rs`、`glyph_atlas.rs`、`renderer/tile_renderer/paint.rs`、`crates/alto-gpu/shaders/glyph-q30.wgsl`（新規。01 の RV4-1 が作る。06 はガンマなどの式を足す）、`crates/alto-gpu/src/cpu_composite/text_tests.rs`、`tests/e2e/cpu-composite.js`、`tests/e2e/parity-tests.js`、`packages/alto-sdk/src/types.ts`、`tests/parity/` に v4 用のケース。Create `crates/alto-gpu/src/glyph_gamma.rs`（新規）。

**Interfaces:** Consumes RV4-1 の `render_version` 4。Produces `DocumentMeta.text_rendering`、`TextRendering`。

- [ ] 試験 `v3_bake_bytes_are_unchanged`（既存の3テキストを焼き直し、`tests/parity/text-bake.txt` の値と実行時に比べる）、`v3_hashes_do_not_change`（v3 文書の描画ハッシュが既存の固定値と一致。固定ファイルを再生成しない）。完了時に `git diff tests/parity` が既存行を触らないことも確かめる。
- [ ] parity ケース `text-v4-hint`、`text-v4-stem`、`text-v4-gamma`（Metal、lavapipe、SwiftShader で全バイト一致）。
- [ ] 試験 `hinting_keeps_advances`（y のみを変え、x と組版の位置を変えない）、`hinting_ignores_rotated_glyphs`、`gamma_table_is_the_committed_one`、`stem_darken_zero_is_identity`。
- [ ] LCD の試験 `lcd_is_off_over_transparency`、`lcd_never_changes_a_hash`、書き出しで LCD が使われないこと。
- [ ] 完了条件・検証：`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`、3バックエンドの parity。PR は項目（ヒント、強調、ガンマ、LCD）ごとに分ける。

### 依存

- 先に：RV4-1（01）、COL-4（01。リニア合成との組み合わせ）。
- 後に：なし。

## TXT-11 縦組み中の RTL

### 現状

- 縦組みの bidi は無効：`shape_paragraph` は `vertical` のとき `levels = vec![0; para.len()]`。横倒し（`Mode::Sideways`）のラテン文字は LTR で整形され、アラビア文字やヘブライ文字の連続も LTR として上から下へ並ぶ。
- 行内の並び替え（`layout` の `order`）は、`levels` の奇数のレベルの連続を逆順にする処理で、横組みの bidi 用に既に書いてあり、`cluster.level` を見るだけで進行方向に依らない。
- `place` の `Mode::Sideways` は、整形の出力順（`Piece.pen`）のとおり進行方向に並べる。`rtl` のランは `Direction::RightToLeft` で整形し、出力は左から右の見た目の順になる。
- 試験：`bidi_runs_are_reordered_visually`（`crates/alto-text/tests/layout.rs`）は横組みのヘブライ文字で、`ClusterBox` の位置と `rtl` だけを調べる。テスト用のフォントにヘブライ文字の字形は無いので、新しい試験も同じく位置を調べる。

### 11-04 からの変化

- 変化なし。旧設計の「90 度回転して上から下へ置く」は、CSS（`writing-mode: vertical-rl`）の挙動と食い違う：右から左の連続は、回転した状態で下から上へ読む（連続の最初の文字が下）。ここではこの向きを正とする（実装前に、ブラウザの縦組みの表示で確かめる）。

### 設計

- 縦組みでも `unicode_bidi::BidiInfo::new(para, Some(Level::ltr()))` で段落を左から右として解析する。段落のレベルは 0 のまま（`para_rtl` は縦組みで偽のまま）。
- 書記素の向き（`Mode`）が `Upright`、`Tcy` のものはレベルを 0 に落とす（漢字、かな、縦中横は bidi に参加しない）。`Sideways` のものだけ bidi のレベルを使う。ランの鍵（`Key.level`）にレベルが入っているので、レベルが違えばランが分かれる。
- 整形：`Sideways` で奇数のレベルのランは `Direction::RightToLeft` で整形する（既存の `rtl` の経路）。行内の並び替えも既存の `order` が働く。直立の文字（漢字）に挟まれた短い RTL の連続は、それぞれ独立に逆順になり、連続の外側の順序は保たれる。
- `place`：変更は要らない見込み（整形の出力順をそのまま進行方向に置く）。確かめる。
- 行頭禁則などの `breaks` は論理順で、変えない。キャレット、ヒットテスト（`select.rs`）は `ClusterBox.rtl` と `inline` を使い、縦組みの RTL の連続でキャレットの向きが逆になる。試験で確かめる。
- 数字（`AN`、`EN`）：縦組みの `auto_tcy` は先に縦中横にしてレベル 0 にする。残りの数字は bidi の規則に従う。

### 形式と通信

- `format_version`：上げない（文書の形は変わらない）。`PROTOCOL`：上げない。ALTB と描画：変えない。
- 既存の縦組みの文書で、アラビア文字やヘブライ文字を含むものは、同じ入力が別のバイトに焼かれる。`text-bake.txt` の既存3行は含まないので不変。保存済みのベイクはバイト列として有効なままで、そのまま描かれる（`BAKER_VERSION` は診断用で、描画も鍵も読まない）。再焼き込みは、そのテキストを編集したとき、またはフォントを置き換えたときに起こる。一括の再焼き込みは作らない（既存の `bake_ops` の方針どおり）。同じ入力が別のバイトになるので `BAKER_VERSION` は上げる（値は実装時に決める）。
- `text-bake.txt` に `text-v3-vertical-rtl`（新規）を足す。

### 作業手順

**Files:** Modify `crates/alto-text/src/layout.rs`、`crates/alto-text/tests/layout.rs`、`crates/alto-engine/src/text.rs`（`BAKER_VERSION`）、`tests/parity/text-bake.txt`、`tests/e2e/m7-tests.js`。

**Interfaces:** なし（内部の変更）。

- [ ] 試験 `rtl_run_in_vertical_keeps_its_order`（ヘブライ文字の連続で、先頭の文字の `ClusterBox` がもっとも下、`rtl` が真）、`vertical_upright_text_ignores_bidi`、`rtl_run_between_kanji_reorders_only_itself`、`vertical_rtl_caret_points_the_right_way`。
- [ ] 既存の縦組みの試験（`vertical_*`、`tate_chu_yoko_*`）が不変。
- [ ] 完了条件・検証：TXT-4 と同じ。`BAKER_VERSION` を上げた理由を書く。PR。

### 依存

- 先に：なし（段階2）。
- 後に：なし。

## Review Focus

- 範囲の追従で、ルビの親文字が削除されたらルビも消えること（`RangePolicy::Group`）。TypeScript と Rust が同じ表で一致すること。
- 焼き込みの最大サイズ。ルビと圏点の字形で字形数とバイト数の上限（共通の設計）を超える文書を `RESOURCE_LIMIT` で拒否し、状態が変わらないこと。
- `merge` と差分。`ContentDetail::Text` が新しいフィールドを見ること（`crates/alto-doc/src/version.rs`、`crates/alto-git/src/lib.rs`）。
- 既存のカーソル移動が、ルビなどの追加で UTF-16 の添字を変えないこと（`ClusterBox` を増やさない）。
- フォント削除の検査（`Document::validate` の `font_ids()`）。ルビ、圏点、下線は親のスタイルのフォントだけを使い、新しいフォントの参照を作らないこと。
- 新しいフィールドの既定値が直列化に出ず、既存の文書のダイジェストと `text_entry` の鍵が変わらないこと。
- `format_version` と `PROTOCOL` の番号上げが、同じ時期の PR でまとまっていること。
