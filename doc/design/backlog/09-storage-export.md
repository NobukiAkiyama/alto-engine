# 保存と書き出し

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 大きな文書をメモリに載せきらずに扱い、書き出しと外部保存先を増やす。

**Architecture:** 正本は内容アドレスのオブジェクト。`alto-raster` の `TileStore` は圧縮オブジェクトの同期 API で、WASM のメモリにある前提。ここを非同期の取得へ広げるのが最大の変更である。

## 現状

- `TileStore::object(&self, hash) -> Option<Arc<StoredTile>>` は同期。`insert_deferred` で検証を遅らせる読み込みがある。
- `versions/lazy.rs`：`openFolder(provider, { history: "on-demand" })` は過去の版のオブジェクトだけを必要時に読む。読む前に `missingObjects`、`provideObjects` の往復を挟む。
- ジャーナルは**名前空間ごと**。`storage.namespace` で別の OPFS フォルダになり、Web Locks で排他する（`recovery-store.ts`）。1エンジン1文書。
- 書き出しは `exportImage`：`convertToBlob` で PNG と WebP。全体を WASM 外のメモリへ集める。64M 画素と 512 MiB の上限。
- `read_entries` は ZIP の全エントリを読む。
- 読み込みのハッシュ不一致や欠損はエラーで、文書を開かない。

## STO-1 作業文書の部分読み込み

**課題。** renderer と画像編集は同期でタイルを読む。非同期にすると全経路に影響する。

**設計。** 既存の遅延の形（先に揃えてから同期処理）を一般化する。

1. **フレームの先取り。** `FramePlanner` の可視タイルから必要なハッシュを列挙する。不足があれば要求し、届くまで粗いレベルか透明のプレースホルダーを出す。到着で再描画する（既存の非同期読み込みの「pending」と同じ）。
2. **編集の先取り。** 編集のたびに触るタイルを列挙する `required_objects(edit) -> Vec<ContentHash>` を用意し、`withObjects` の形で先に読む。既存の `ObjectTarget` に `Edit`、`Region` を追加する。
3. **常駐の管理。** 常駐は「作業集合」。保存済みで現在の範囲外のタイルは捨てられる。未保存のタイルは捨てない（ジャーナルが保持していても、再構築の経路を増やさない）。予算は 1 GiB（設計書 5.1）、超えたら LRU で捨てる。
4. **読み出し。** `.altoproj` の provider と、`.alto` の ZIP の中央ディレクトリからの範囲読み（`Blob.slice`）。`read_entries` は全読み込みなので、新規に `ZipIndex` を作る。

**整合性。** 読んだオブジェクトは常に内容ハッシュで検証する。保存先のファイルが読み取り中に置き換わった場合は `CONFLICT`。`createWritable` で外部に上書きされたらハンドルの読み取りが失敗するので、開いた状態を「切断」にする。

**試験の条件。** 実メモリの上限を超える文書で、パン、ブラシ、Undo、保存、GPU 喪失が動くこと。

## STO-2 文書別ジャーナル

**設計。** 名前空間で既に分かれている。残りは次の2点のみ。

- `createEngine({ storage: { namespace: "auto" } })`：UUID の名前空間を生成し、`recovery-store.ts` の一覧から復旧を選べるようにする。
- 復旧の一覧に文書名と最終更新を出す。

`MILESTONES.md` の「オリジン当たり1作業領域」の記述は名前空間を反映しておらず、本項目の実装と同時に直す。

## STO-3 JPEG、TIFF 書き出し

**設計。**

- JPEG：`convertToBlob({ type: "image/jpeg", quality })`。アルファは白（既定）または指定色に合成してから渡す。
- TIFF：`alto-store` に無圧縮または Deflate の RGBA8 ベースライン TIFF エンコーダーを書く（新規依存なし）。Worker で実行。4 GiB 未満の制約から 64M 画素は収まる。

`ExportImageOptions` に `type: "image/jpeg" | "image/tiff"`、`quality?: number`、`background?: Color`。

## STO-4 逐次エンコーダー

**設計。** PNG は行ごとの IDAT で逐次に書ける。`exportImage` の新しい経路 `exportImageStream(options): ReadableStream<Uint8Array>` を追加する。タイルの行を順に読み戻して、PNG の行フィルタ（なし）と Deflate に通す。CompressionStream を使う。64M 画素の上限を外し、文書の辺 65536 まで。メモリの上限は同時に保持する行数で決める。WebP と JPEG は Canvas 依存のため別扱い（既存）。

## STO-5 保存ファイルへのプレビュー格納

**設計。** `.alto` に `preview.png`（長辺 512）を入れる。manifest に `preview: { path, hash, snapshot }` を記録し、snapshot が一致しないプレビューは無視する。無ければ何も壊れない。ファイル一覧の表示に使える。圧縮オブジェクトの扱いを変えない。

## STO-6 派生 OPFS キャッシュ

**設計。** [COLD-RENDERING-DESIGN.md](../COLD-RENDERING-DESIGN.md) の案 C のとおり、完成した表示タイルを別の領域へ保存する。鍵は「スナップショットの内容ハッシュ × キャッシュキー」。破損や容量不足はキャッシュの無効として黙って扱い、正本に影響しない。別の Cache Worker を置く。測定で命中時の短縮を確かめてから採用する（STO-1 の後に測る）。

## STO-7 ステージング退避、未確定ストロークの復元

**設計。** ストローク中の入力サンプルをジャーナルへ先に書き（`stroke_samples` レコード）、復旧時は読み戻しの代わりに、同じダブ列を確定の入力として再実行する。ただし GPU 間で結果が違うため、「復旧した結果は当時の画面と同一でない」という扱いで、新しい Transaction として確定する。Undo で戻せる。CPU のスタンプ（`stroke_q8`）で確定できる範囲に限る。テクスチャ先端（BR-3）は対象外。

## STO-8 最近使ったファイル

**設計。** IndexedDB に `FileSystemHandle` を保存する SDK の補助。`@alto/engine/browser` の `recentFiles()`、`rememberFile(handle)`、`forgetFile(id)`。件数は 32。権限は開くときに `requestPermission`。本体には入れない。

## STO-9 タイル表の別オブジェクト化

**設計。** 設計書 4.1 の案。1レイヤー 1024 タイルを超えたら、タイル表を内容アドレスのオブジェクトに分け、スナップショットはそのハッシュと件数を持つ。JSON の差分が読みやすいまま、4K 超の文書でスナップショットが膨らまない。GC と差分（`DiffItem`）の根に含める。format を上げる。

## STO-10 破損タイルの部分表示

**設計。** 設計書の方針（壊れたタイルは欠損として赤い格子で表示し、報告して開く）は未実装で、現在はエラーで開かない。この「拒否」は安全側である。

- `engine.open(source, { onCorrupt: "refuse" | "placeholder" })`。既定は `refuse`。
- `placeholder`：不足と破損のタイルを `missing` として記録し、読み取り専用で開く。保存は拒否する（欠損が別の版へ広がらないように）。`doc.exportImage` と「別名でコピー可能なタイルのみ保存」は許可する。
- 表示：表示経路の専用シェーダーで格子を描く。正本には書かない。

**manifest の破損：** 設計書の「有効な最新スナップショットを探す」は、`snapshots/` の列挙から順に試す `recoverFromObjects`。提案として返し、自動では開かない。

## STO-11 PSD 往復

**設計。** `alto-psd` を別 crate にし、読み込みと書き出しを `Op` で実現する。

- 読み込み：RGB 8bit、レイヤー、マスク、グループ、不透明度、合成モードの一致するもの。テキストはラスターの結果を使う。非対応要素は警告の一覧で返す。CMYK は拒否。
- 書き出し：v3 の合成の見た目は Photoshop の結果と画素単位で一致しないことを前提にする。効果は焼き込みで、編集可能では残さない。
- ライセンス上、仕様公開の範囲で書く。第三者の crate を使う場合は `THIRD-PARTY-NOTICES.md` を更新する。

## STO-12 外部保存先

**設計。** `ObjectStorageProvider` を実装するだけでエンジンの変更は不要、という境界は既にある。OAuth と外部送信を SDK 本体へ入れないため、別パッケージ `packages/alto-storage-gdrive` にする。トークンの取得は呼び出し側が行い、`getAccessToken` を渡す。

- `objects/` はハッシュ名のファイル、`manifest.json` は Drive の revision を楽観ロックに使う。
- Drive は同名のファイルを作れるので、`list` と `getObject` はファイル名の重複を検出して内容ハッシュで確かめる。
- `gcCapabilities` は持たない（共有の排他を保証できない）ので、GC は確認のみになる。
- 実際のアカウントでの試験は利用者の環境でのみ行う。CI は Drive API の模擬で試験する。

## 実装タスク

### Task STO-3: JPEG と TIFF

**Files:** Modify `packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/worker.ts`。Create `crates/alto-store/src/tiff.rs`。

**Interfaces:** `exportImage({ type, quality, background })`。`alto_store::tiff::encode_rgba8(width, height, rgba: &[u8]) -> Vec<u8>`。

- [ ] 試験 `tiff_roundtrip_header_fields_are_valid`、`tiff_size_mismatch_is_refused`。
- [ ] ブラウザ試験 `jpeg export composites alpha over the background`、`quality outside 0..1 is invalid`。
- [ ] 実装、PR。

### Task STO-8: 最近使ったファイル

**Files:** Modify `packages/alto-sdk/src/browser.ts`、`packages/alto-sdk/api`。

- [ ] ブラウザ試験 `recent files keep the newest 32`、`a revoked permission reports without removing the entry`。
- [ ] 実装、PR。

### Task STO-2: 名前空間の自動生成

**Files:** Modify `packages/alto-sdk/src/engine.ts`、`recovery-store.ts`、`doc/MILESTONES.md`。

- [ ] ブラウザ試験 `auto namespaces let two documents journal at once`、`recovery lists both`。
- [ ] 実装、PR。

### Task STO-9: タイル表のオブジェクト化

**Files:** Modify `crates/alto-doc/src/lib.rs`、`crates/alto-store/src/lib.rs`、`crates/alto-engine/src/versions/gc.rs`。

- [ ] 試験 `tile_table_over_1024_uses_an_object`、`gc_keeps_the_table_object`。
- [ ] 試験 `diff_reads_tables_without_loading_pixels`。
- [ ] 実装、PR。

### Task STO-1: 部分読み込み

複数 PR に分ける。

**Interfaces:** Produces `ZipIndex::read(source) -> ZipIndex`、`Engine::required_objects(edit) -> Vec<ContentHash>`、`ObjectTarget::{Edit, Region}`。

- [ ] PR 1：`ZipIndex`。試験 `zip_index_reads_one_object_without_the_rest`、`truncated_central_directory_is_corrupt`。QA-1 のファザーに追加。
- [ ] PR 2：`required_objects`。試験 `required_objects_cover_every_read_of_an_edit`（編集を読み取り追跡付きのストアで実行し、列挙から漏れた読み取りがないことを確かめる）。
- [ ] PR 3：フレームの先取りと常駐。ブラウザ試験 `pan loads tiles on demand`、`a tile arrives after the view moved`、`GPU loss with evicted tiles rebuilds`。
- [ ] PR 4：保存の整合性。試験 `save_keeps_evicted_objects_reachable`。
- [ ] 実メモリを超える文書で、B1〜B7 と長時間の測定を記録。

### Task STO-10: 破損タイルの部分表示

- [ ] 試験 `refuse_is_the_default`、`placeholder_opens_read_only_and_refuses_save`。
- [ ] ブラウザ試験 `a corrupt tile is drawn as a grid and reported`。
- [ ] 実装、PR。

### Task STO-4、STO-5、STO-6、STO-7

- [ ] STO-4：試験 `stream export matches the one shot PNG pixels`、`a cancelled stream releases its readback`。
- [ ] STO-5：試験 `a stale preview is ignored`、`a package without a preview opens`。
- [ ] STO-6：STO-1 の後に、C4 の命中で短縮を測ってから着手。
- [ ] STO-7：試験 `a killed stroke replays as one undoable transaction`、`a textured stroke is not replayed`。

### Task STO-11: PSD

**Files:** Create `crates/alto-psd/`。Modify `crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/engine.ts`、`document.ts`。

**Interfaces:** `engine.openPsd(blob): Promise<{ document, warnings }>`、`doc.exportPsd(): Promise<Blob>`。

- [ ] 試験 `psd_header_and_layer_tree_parse`、`cmyk_psd_converts_or_is_refused_with_a_warning`。
- [ ] 試験 `unsupported_elements_are_listed_not_dropped_silently`。
- [ ] 試験 `export_then_open_keeps_structure_and_pixels`。
- [ ] 検証用の PSD は自作のものを `tests/fixtures/psd/` へ。Photoshop での実機確認は利用者の環境で行い、記録する。
- [ ] QA-1 のファザーに `psd` を追加。
- [ ] 実装、PR。

### Task STO-12: Google Drive

**Files:** Create `packages/alto-storage-gdrive/`。

- [ ] Node 試験（模擬 API）`objects are idempotent`、`manifest write loses the race`、`duplicate names are resolved by hash`、`an expired token surfaces as STORAGE`。
- [ ] `@alto/engine` にネットワーク呼び出しが増えていないことを試験。
- [ ] 実装、PR。

## Review Focus

- 読み込み中に Worker が再起動した場合に、ジャーナルの整合が保たれること。
- 保存先が消えたり権限を失ったりした場合に、常駐から落としたタイルが読めない状況のエラーの明示。編集中の作業は失わない。
- Undo が古いタイルを要求するときの読み込み待ちの間に、次の編集が来た場合の順序。
- `.alto` の範囲読みで、別プロセスがファイルを置き換えていないことの確認。
- ハッシュ検証を省略する経路を作らないこと。
