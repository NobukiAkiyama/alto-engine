# 保存と書き出し

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 大きな文書をメモリに載せきらずに扱い、書き出しと外部保存先を増やす。

**Architecture:** 正本は内容アドレスのオブジェクト（`objects/<2>/<hash>`、`ALTOOBJ1` の LZ4）。エンジンのメモリでは `alto-raster` の `ObjectStore` が圧縮タイルを持ち、`TileStore::object` は同期で引く。描画（`alto-gpu`）も CPU の編集（`alto-engine`）も、この同期の読み出しを前提にしている。F-9（版を `.alto` の外に持つ）までで、過去の版のタイルは「必要な操作の直前に読む」形になった。作業文書のタイルは今も全部メモリにある。ここを広げるのが STO-1 で、最大の変更である。

## 共通の現状

項目をまたいで使う事実をここにまとめる。各項目の「現状」は差分だけを書く。

**保存の形（F-9 の後）。**

- 3つの形がある。普通の `.alto`（ZIP、manifest format 2）、フォルダ形式 `.altoproj`（同じ論理エントリを1ファイルずつ。format 2）、薄い `.alto`（manifest format 3）。
- 薄い `.alto` は、作業文書のオブジェクト、全てのスナップショットと commit、`external-objects.json`（版だけが参照するオブジェクトのハッシュの辞書順の配列）を持つ。manifest の `external_history` は `document`（根の NodeId）、`objects`（件数）、`digest`（`external-objects.json` の BLAKE3）。型は `alto_store::Manifest`、`ExternalHistory`、`ExternalObjects`（`crates/alto-store/src/lib.rs`）。`Manifest` は `deny_unknown_fields`。
- 読み手（`decode_entries` の中の検査）は `format` 2 で `external_history` 無し、または `format` 3 で `external_history` 有りだけを受ける。`document_format` は 5 だけを受ける。ほかは `UNSUPPORTED`。manifest、スナップショット、commit、`objects/` 以外のエントリは読まずに無視する。
- 壊れたオブジェクト（復号できない、内容ハッシュが合わない）は警告に集めてから、まとめて `CORRUPT_DATA` で拒否する。文書は開かない。
- `.alto` の上限は `MAX_PACKAGE_BYTES`（512 MiB。エントリのデータの合計）。書くときは `encode_parts_fetching` が、読むときは `read_entries` が同じ値で止める。エントリ数は 1,000,000 まで。上限を超えた書き込みは `RESOURCE_LIMIT: package exceeds 512 MiB`（F-3）。フォルダ形式に上限はない。

**SDK の入口（`packages/alto-sdk/src/engine.ts`、`document.ts`）。**

- `engine.open(source, { history? })`：`ArchiveSource.read()` の Blob を全部 `arrayBuffer()` にして Worker へ渡す。薄い `.alto` は `history`（版の置き場）を確かめてつなぐ。満たさなければ警告 `HISTORY_STORE_UNAVAILABLE`（reason `missing`、`other-document`）で開く。
- `engine.openFolder(provider, { history: "all" | "on-demand" })`：`on-demand` は作業文書のオブジェクトだけを読み、`list("objects/")` を known として渡す。
- `doc.save(target, { history: "all" | "omit" | { folder } })`、`doc.saveFolder(provider)`、`doc.packageSize({ history })`。`save` の既定と `{ folder }`、`saveFolder`、`packageSize` は編集キュー（`edit`）の中で動く。`save(target, { history: "omit" })` だけは `saveWithoutHistory` を編集キューの外で直接要求する（`document.ts`）。`saveFolder` を編集キューの中で行う修正（PR #124）で、保存の途中に commit が割り込まない。
- `withObjects(target, run)`：`missingObjects` → 置き場から読む → `provideObjects`（全件を検証してから取り込み、pin）→ `run` → `releaseObjects`。置き場が無ければ `STORAGE`（reason `objects-unavailable`）。`ObjectTarget` は `commit`、`branch`、`merge`、`merge_plan`、`all` の5つ（`crates/alto-engine/src/versions/lazy.rs`、serde の `type` タグ）。
- 置き場の共有ロック：`ObjectStorageProvider.holdObjects` を開いている間持ち、他の文書の GC から守る。`gcCapabilities` が無い保存先は GC が確認だけになる。

**エンジンの常駐（`crates/alto-engine/src/lib.rs` の `Engine::maintain`）。**

- 集合は「現在」（各文書のノードとアセット、取り込み中、ステージ中）と「到達」（現在に、版の `retained` を足したもの）。
- `LazyObjects`（`known`、`pinned`、`external_history`）を持つ文書では、known にあって現在に無いオブジェクトはメモリから外す。**現在（作業文書）のオブジェクトは known にあっても外さない。**
- 保存か読み込みの後（`backed`）は、現在に無く到達にあるタイルを `ObjectStore::separate` で cold（メモリ上の別の表）に移す。cold もメモリにある。
- `mark_folder_saved` は保存した置き場の中身（文書と版の全参照）を known にする。`mark_store_saved` は薄い保存の後に、全部を持っていた文書を「必要な分だけ読む」状態にする。
- Undo / Redo だけが参照するオブジェクトは `MAX_HISTORY_ONLY_OBJECTS`（2048）を超えると古い Undo から捨てる。復号キャッシュ `DecodeCache` は 128 MiB。

**復旧（`packages/alto-sdk/src/storage-core.ts`、`worker.ts`）。**

- 名前空間ごとに OPFS のフォルダ `alto-engine/ns-<UTF-8 の16進>` を持ち、2本のジャーナル（`journal-a.bin`、`journal-b.bin`）と `packs/` を置く。Web Locks `alto-engine:<namespace>` で1エンジンに限る。
- チェックポイントは、ストアが持たないオブジェクトだけを新しい pack に書く。Worker の `Runtime`（`crates/alto-wasm/src/lib.rs`）は `stored_objects` でストアにあるものを覚える。F-7 でセッション中に pack を片付けるようになり、消したオブジェクトは `forgetStoredObjectPaths` で忘れさせる。
- 置き場から必要な分だけ読む文書のチェックポイントは、known の中身を持たず `lazy-objects.json` に known を書く。ページを読み直した後は `versions.attachFolder` が要る。
- 編集のジャーナル記録は `Engine::journal_record`（ZIP。`transaction.json` と新しいオブジェクト）。ストローク中の入力はジャーナルに書かず、`endBrush` の確定で1件書く。

**書き出し（`packages/alto-sdk/src/document.ts` の `exportImage` → Worker の `exportImage`）。**

- 表示タイル（ピラミッドの段 `level`）を `readTiles` で `readbackCapacity` 件ずつ読み戻し、非乗算にする。等倍（または段の画素と同じ大きさ）の PNG は SDK の `encodePng`（`packages/alto-sdk/src/png.ts`。8bit RGBA、`sRGB` chunk、Paeth、`CompressionStream("deflate")`）で符号化する。拡大縮小と WebP は `OffscreenCanvas` の `convertToBlob`。
- 上限：段の画素で 64,000,000 画素（`INVALID_ARGUMENT: export exceeds 64M pixels`）。作業量の見積り `(段の画素 + 出力画素) × 8 + readbackCapacity × 262144 × 3` が 512 MiB を超えると `INVALID_ARGUMENT: export CPU operation exceeds 512 MiB`。`region` の NaN は `INVALID_ARGUMENT`（reason `region`）、`scale` は 0 より大きく 1 以下（reason `scale`）。契約は [EXPORT-CONTRACT.md](../EXPORT-CONTRACT.md)。

**描画の読み出し（`crates/alto-gpu/src/renderer/`）。**

- 11-04 版の FramePlanner は無い。フレームは `Renderer::render`（`frame.rs`）が表示の段と可視の表示タイル（`visible_keys`）を決め、表示タイルごとの `TileJob`（`jobs.rs`）が入力のハッシュを集め（`TileJob::plan_one`。画素は読まない）、`Renderer::upload_timed`（`upload.rs`）が `alto_raster::load` でストアから引いてアトラスへ上げる。M5 の合成器（`tile_renderer/targets.rs`）も同じ `upload` を通る。
- ストアに無いハッシュは `job.missing` に入り、アトラスの slot `u32::MAX`（透明）として合成される。**今は「無い」と「まだ読んでいない」を区別しない。**
- ストロークの GPU 経路（`renderer/stroke.rs`）は、下地のタイルが無いと透明の下地から描く。

## STO-1 作業文書の部分読み込み

### 現状

- 作業文書のタイルは開いた時点で全部メモリにある。`openFolder(..., { history: "on-demand" })` と薄い `.alto` で外に置けるのは版だけのオブジェクトで、`Engine::maintain` は現在のオブジェクトを known でも外さない。
- `engine.open` は Blob を全部読む（最大 512 MiB）。`read_entries` は ZIP の全エントリを読む。範囲読みの仕組みは無い。
- 描画は `TileStore::object`（同期）で引き、無ければ透明（共通の現状）。CPU の編集は `Engine::tile`（無ければ `None`）と `Engine::require_tile`（無ければ `MissingTile`、`NOT_FOUND`）を使い分ける。ストロークの GPU 経路は下地が無いと透明から描く。どれも「読んでいないだけ」の場合に誤った画素を作る経路がある。
- 置き場から読む仕組みは `withObjects`（SDK）と `missing_objects`／`provide_objects`／`release_objects`（`lazy.rs`）。対象は版の単位（`ObjectTarget`）で、レイヤーや範囲の単位は無い。
- 復旧のストアは、最初のチェックポイントの後は作業文書のオブジェクトを全部持つ（pack に1回ずつ）。`Runtime` は `stored_objects` でそれを知っている。ストアからオブジェクトを1件ずつ読む要求は無い（`loadCandidate` は候補のパッケージを丸ごと組み立てる）。

### 11-04 からの変化

- FramePlanner は無い（上の「描画の読み出し」）。`renderer.rs` は 725d6f4 で `frame.rs`、`jobs.rs`、`upload.rs` などに分かれた。フレームの先取りは `TileJob` の入力（`unique`）と `upload` を起点に設計し直した。
- F-9（8c6d2fd〜0fbf4ec）で、薄い `.alto`、`mark_store_saved`、`check_history_store`、`version_only_objects`、`HISTORY_STORE_UNAVAILABLE` が入った。F-9 の設計は「作業文書のタイルを外に出すこと」を対象外とし、本項目に残した。known と pin の仕組みは F-9 と共有する。
- F-5（1e6096f）で `.alto` の保存がオブジェクトを1件ずつ取り出して書く形（`encode_parts_fetching`）になった。F-7（9755611 ほか）で復旧のストアの pack が片付けられるようになった。外した作業文書のオブジェクトを復旧のストアから読み戻す設計は、この片付けと矛盾しないことを確かめる必要がある（下の「設計」4）。
- 11-04 版の「予算は 1 GiB（設計書 5.1）」の出典はリポジトリに無い。暫定値として扱う（下）。

### 設計

明示した場合だけ働く。既定の `open`、`openFolder` は今のまま全部読む（F-9 と同じく、失敗の現れ方が変わるため）。

**1. 用語と不変条件。**

- **readable**：メモリに無くても読み戻せるオブジェクト。`known`（開いた置き場か `.alto` にある）か、復旧のストアにある（`stored_objects`）もの。
- **pending**：文書が参照し、メモリに無く、readable なもの。描画と編集は pending を「透明」として扱ってはならない。
- 不変条件：**pending を読もうとした処理は、画素を作らずに止まる。** 描画はその表示タイルを未完成として扱い、編集は文書を変えずに失敗する。readable でない欠けは今のとおり破損（`CORRUPT_DATA`）。

**2. 常駐の管理（Rust）。**

```rust
// crates/alto-engine/src/residency.rs（新規）
pub struct Residency {             // （新規）
    pub budget_bytes: u64,         // 圧縮バイトの予算
    used: BTreeMap<ContentHash, u64>, // 最後に使ったフレーム／操作の番号（LRU）
    pins: BTreeMap<PinOwner, BTreeSet<ContentHash>>,
}
pub enum PinOwner { Operation, Stroke, Frame } // （新規）
pub const DEFAULT_RESIDENT_BYTES: u64 = 1 << 30; // （新規）暫定。測定で決める
```

- `Engine::maintain` に1段を足す：現在のオブジェクトのうち、readable で、pin されておらず、未保存の変更を持たない（`staged_tiles`、取り込み中、Undo だけが参照するものは外さない）ものを、圧縮バイトの合計が予算を超える分だけ LRU で外す。今の `ObjectStore` には1件ずつ外す API が無い（`separate` は到達するものを cold に移すだけで、cold もメモリにある）。`ObjectStore::evict(&mut self, hashes: &BTreeSet<ContentHash>)`（新規）を足し、`tiles` と `cold` の両方から消す。`separate` は今のまま使い、その後に `evict` を呼ぶ。
- 外すのは予算を超えたときだけ。予算内なら今と同じ。
- 読み戻したオブジェクトは `provide_objects` と同じく内容ハッシュを検証してから入れる。検証を省く経路を作らない（`insert_deferred` は復旧のチェックポイントにだけ使う今の規則を保つ）。
- `TileStore` に既定の実装つきの `fn pending(&self, hash: &ContentHash) -> bool { false }` を足す（新規）。今の `TileStore` は `object` と `is_opaque` だけで、`ObjectStore::object` は「無い」を `None` で返すので、「未読込」と「欠損」を区別する経路は無い。区別は状態を別に持たずに、毎回次の式で求める（`maintain`、`provide_objects`、外す処理の前後で食い違わない）（新規）：
  `pending(h) = 現在の文書が参照する && !tiles.reachable(h) && (いずれかの LazyObjects.known に h がある || recovery_stored に h がある)`。
  `recovery_stored`（新規）は `Engine` に足す集合で、`Runtime` の `stored_objects` を写す（`Runtime` がチェックポイントの成功と `forgetStoredObjectPaths` のたびに `Engine::set_recovery_stored`（新規）で更新する）。`provide_objects` で入ったものは `reachable` が真になり pending でなくなる。式が偽で `object` が `None` のものは今どおり欠損（破損）。
- 予算の SDK：`createEngine({ memory: { residentMB } })`（新規）。既定は `DEFAULT_RESIDENT_BYTES`、64〜4096 の整数、範囲外は `INVALID_ARGUMENT`）。予算が文書に足りないときの扱いは 6. の上限による。

**3. フレームの先取り（描画）。**

- `Renderer::upload_timed` と `Renderer::upload` は、`load` が `None` で `store.pending(hash)` が真のとき、`u32::MAX` を返す前にそのハッシュを `wanted: BTreeSet<ContentHash>`（新規）に入れる。
- そのジョブの合成結果は「未完成」として表示キャッシュに載せる（`Cached.incomplete: bool`（新規。`cache_map` の値の型 `Cached` に足す））。未完成の表示タイルは提示はするが、`FrameReport.pending` に数え、`incomplete_inputs`（新規）を持つ。入力が届いたら未完成の表示タイルだけを作り直す。
- 表示：未完成の表示タイルは、既にある古い表示（`hasStaleFallback` の経路）か、上の段の表示タイルを拡大して出す。どちらも無い場合だけ透明。新しいシェーダーは足さない。
- `Renderer::take_wanted() -> Vec<ContentHash>`（新規）。Worker（`worker.ts`）は毎フレームの後に呼び、`stored_objects` にあるものは復旧のストアから、残りはページに頼んで置き場から読む（下の 4.）。届いたら `provideObjects`（対象 `resident`）で入れ、フレームを要求する。
- フレームで pin するのは可視の表示タイルの入力だけ（`PinOwner::Frame`）。次のフレームで入れ替える。

**4. 読み出しの元。**

| 元 | 読み方 | いつ使う |
|---|---|---|
| 復旧のストア | Storage Worker の新しい要求 `readStored { paths }`（新規）。pack の範囲を読む | `stored_objects` にあるもの。最初に試す |
| `.altoproj`（`ObjectStorageProvider`） | 今の `getObject`。`exclusive` があればその中 | known にあり、置き場がつながっているもの |
| `.alto` | `Blob.slice` で範囲を読む。中央ディレクトリは `ZipIndex`（STO-5 で作る `packages/alto-sdk/src/zip-index.ts`（新規）） | `engine.open(source, { tiles: "on-demand" })` で開いたもの |

- F-7 の pack の片付けと整合させる：外したオブジェクトが入っている pack は、最新のチェックポイントが参照する限り片付けの対象にならない（今の `collectPlan` の規則）。外したオブジェクトを `stored_objects` から忘れさせるとき（F-7 の `forgetStoredObjectPaths`）は、そのオブジェクトがメモリに無ければ先に読み戻して pin し、次のチェックポイントで書き直す。読み戻せなければ外したオブジェクトを持つ pack を消さない（`collect` から除く）。
- 復旧のストアを持たないエンジン（`storage.recovery: false`）は、置き場か `.alto` だけを元にする。元がつながっていない pending は、編集を `STORAGE`（reason `objects-unavailable`）で止め、表示は未完成のまま。

**5. 編集の先取り。**

- SDK は、画素を読む編集（`imageEdit`、`canvasEdit`、`layerEdit`、選択の `select`、`setMask`、`setEffects` の選択範囲、版の操作）の前に `withObjects` を通す。対象を2つ足す：

```rust
pub enum ObjectTarget {
    Commit { hash: ContentHash }, Branch { name: String }, Merge { name: String }, MergePlan, All,
    Nodes { nodes: Vec<NodeId> },                  // （新規）ノードのタイル、マスク、効果の選択範囲
    Region { nodes: Vec<NodeId>, rect: IRect },    // （新規）文書座標の範囲に掛かるタイルだけ
}
```

- `Engine::required_objects(h, edit: &serde_json::Value) -> Result<ObjectTarget, EngineError>`（新規）：編集の JSON から対象を決める。範囲が決まる編集（長方形の塗り、範囲の効果など）は `Region`、決まらないもの（レイヤー全体の変形、拡縮、選択の `color` など）は `Nodes`。
- 守りは Rust 側に置く：`Engine::tile` と `Engine::require_tile` は pending のハッシュに `STORAGE`（reason `objects-unavailable`）を返す（新規の分岐）。メッセージに足りないパスを入れ、SDK はそれを読んで1回だけ読み直して再実行する。2回目も足りなければ失敗を返す。編集は検証してから適用するので（共通規則4）、途中で止まっても文書は変わらない。これを確かめる試験を作業手順に置く。
- ストローク：`doc.tools.begin("brush", ...)` は対象レイヤー、選択、マスクのタイルを `Nodes` で読み、`PinOwner::Stroke` で `endBrush`／`cancelBrush` まで pin する。pin の合計が予算の半分を超えると `RESOURCE_LIMIT`（`"brush layer exceeds the resident budget"`）で始めない。`renderer/stroke.rs` は下地のハッシュが pending なら透明から描かず `RenderError` で止める（守り）。範囲だけを pin する案は測定の後に決める。
- Undo / Redo が戻すタイルは今も Undo の記録が参照するので外さない（2. の規則）。版の操作は今の `withObjects` のまま。

**6. 上限とエラー。**

- 予算：`DEFAULT_RESIDENT_BYTES`（暫定 1 GiB）。WASM のメモリは 4 GiB が上限で、F-9 の4時間試験は 703 MiB で止まった。予算は測定の後に決め直す（下の判定条件）。（新規）
- 1回の読み出しの件数：`MAX_OBJECTS_PER_READ = 512`（新規）。`provideObjects` の1回の転送もこの件数まで。
- pin の合計が予算を超える操作は `RESOURCE_LIMIT` で始めない（版の操作の `withObjects({ type: "all" })` は今と同じく例外で、`.alto` の上限 512 MiB に縛られる）。
- 元のファイルが読み取り中に置き換わった（`Blob.slice` の読みで `NotReadableError`、または `ZipIndex` の検査で長さが合わない）：`CONFLICT`（reason `source-changed`（新規））。文書は開いたまま、その元を「切断」にして、以後の pending は `objects-unavailable`。
- 読んだバイトの内容ハッシュが合わない：`CORRUPT_DATA`。その後の扱いは STO-10。

**7. 保存。**

- 外した作業文書のオブジェクトは `external_objects` に入れない。`external_objects` は、復旧のチェックポイントが「外にあるので持たない、一覧からも外す」集合として使う（`recovery_checkpoint`。`checkpoint_parts` はこれを `stored_objects` から消す）。`check_history_store`（薄い `.alto` の置き場の確認）と `package_bytes`（空でなければ大きさ不明）も使う。広げると、チェックポイントが作業文書を持たなくなり、健全な薄い `.alto` に `HISTORY_STORE_UNAVAILABLE` が出る。代わりに `Residency` が外した集合 `evicted: BTreeMap<ContentHash, u64>`（新規。値は `encoded_len`）を別に持つ。
- 復旧のストアがあるエンジンでは、外せるのは `stored_objects` にあるものだけにする（チェックポイントは `stored` のバイトを飛ばし、一覧には残すので、自己完結のまま）。復旧のストアが無いエンジン（チェックポイントを書かない）だけ、known にあるものも外せる。
- `package_bytes` と `thin_package_bytes` の `fetch_plan` は、外したオブジェクトの長さを `evicted` から読む（新規）。
- 同じ置き場への `saveFolder`：`folder_entries_keeping_external` に渡す集合を、`external_objects` と「`evicted` のうちその置き場の known にあるもの」の和にする（新規）。外したものは置き場にあるので読まない。
- `save(target, { history: "omit" })`：今は編集キューの外で `save_without_history` が全ての `doc_hashes` を `encode_fetching` するので、外したオブジェクトがあると `MissingTile` で失敗する。編集キューの中に移し、`withObjects` の `nodes`（作業文書の全ノード）で外したものを読み戻してから実行する（新規）。
- 別の置き場への `saveFolder` と `.alto` への `save`：今は `withObjects({ type: "all" })` で全部を読む。`.alto` は 512 MiB が上限なので今のままでよい。別の置き場へは、`MAX_OBJECTS_PER_READ` 件ずつ読んで書き、pin を外してから次へ進む形にする（`Engine::folder_copy_plan`（新規）が書く順のパスを返す）。
- 薄い `.alto` の保存（F-9）：作業文書のオブジェクトを `.alto` に入れるので、外したものは読み戻す。`.alto` の上限は作業文書の大きさで決まる（F-9 と同じ）。

**8. 復旧。**

- 既定（復旧のストアがある）：外すのはストアにあるものだけにできる（readable の定義）。最初のチェックポイントの前に外す必要が出たら、先にチェックポイントを書く。これで、ページを読み直しても作業文書は復旧のストアから戻る（今の振る舞いを保つ）。
- 復旧で開くとき：`loadCandidate` は候補の全オブジェクトを組み立てる。部分読み込みで開いていた文書の復旧は、`loadCandidate` にオブジェクトを含めない形（新しい選択肢 `objects: "on-demand"`（新規））を足し、pending を `readStored` で読む。
- Worker の再起動中は SDK が元（置き場、`.alto` の Blob と `ZipIndex`）を覚えていて、続けて読む。（新規）

**判定条件（測定で形が変わる点）。**

- 予算の既定：実メモリを超える文書（下の試験の文書）で、B1〜B7（`perf-tests.js`）と 40分の長時間試験の WASM メモリを、予算 256 MiB、512 MiB、1 GiB で測り、B1 の p95 が全部読む場合より20%以上悪くならない最小の値を既定にする。
- ストロークの pin の範囲：レイヤー全体の pin で 64M 画素の文書が予算に収まらないと分かったら、範囲の pin（可視範囲 ＋ ブラシ径の余白）に変える。

### 形式と通信

- `format_version`、manifest の `format`：上げない。保存の形は変えない。
- `PROTOCOL`：上げる（11。段階3の回）。`provideObjects` の対象 `resident`、`readStored`（Storage Worker 側）、`open` の `tiles: "on-demand"`、`createEngine` の `memory`、エラーの読み直しの往復。（新規）
- ジャーナル：変えない。チェックポイントの `lazy-objects.json` を使う。`loadCandidate` に選択肢を足す（Storage Worker の内部の要求）。
- 描画の版：触れない。画素は変わらない（未完成の表示タイルは表示だけ）。

### 作業手順

複数 PR に分ける。順に入れる。

**Task STO-1a：守り（読んでいないものを透明にしない）。**

Files：Modify `crates/alto-raster/src/lib.rs`（`TileStore::pending`）、`crates/alto-engine/src/lib.rs`（`Engine::tile`、`require_tile`、`impl TileStore for Engine`）、`crates/alto-gpu/src/renderer/upload.rs`、`crates/alto-gpu/src/renderer/stroke.rs`、`crates/alto-gpu/src/renderer/jobs.rs`、`crates/alto-gpu/src/renderer/frame.rs`（`FrameReport`）。

Interfaces：Produces `TileStore::pending`、`ObjectStore::evict`、`Engine::set_recovery_stored`、`Renderer::take_wanted`、`Cached.incomplete`（新規）。

- [ ] 試験 `pending_tile_is_never_composed_as_transparent`（`crates/alto-gpu/src/renderer/tile_renderer/pool_tests.rs` か新しい試験）：`pending` が真のストアで合成した表示タイルが未完成として残り、届いた後に作り直すと全部ある場合と同じ画素になる。
- [ ] 試験 `cpu_edit_on_pending_tile_fails_and_leaves_the_document`（`crates/alto-engine/src/image/tests.rs`）：pending のタイルを読む塗り、選択の `contiguous`、拡縮が `STORAGE`（reason `objects-unavailable`）で失敗し、`state_digest` が変わらない。
- [ ] 試験 `stroke_over_pending_base_refuses`：下地が pending のストロークが透明から描かずに止まる。
- [ ] 完了条件：pending を持たない今の文書で、既存の試験とパリティがすべて通る（`pending` の既定は `false`）。

**Task STO-1b：常駐の予算と外し方。**

Files：Create `crates/alto-engine/src/residency.rs`（新規）。Modify `crates/alto-raster/src/lib.rs`（`ObjectStore::evict`）、`crates/alto-engine/src/lib.rs`（`maintain`、`recovery_stored`）、`crates/alto-engine/src/versions/lazy.rs`（`external_objects`、`ObjectTarget::Nodes`、`ObjectTarget::Region`、`required_objects`）、`crates/alto-wasm/src/lib.rs`、`crates/alto-wasm/src/versions.rs`。

- [ ] 試験 `eviction_keeps_unsaved_and_undo_only_objects`：未保存のタイル、ステージ中、Undo だけが参照するものは予算を超えても外れない。
- [ ] 試験 `evict_removes_from_tiles_and_cold`（`crates/alto-raster/src/lib.rs`）：`separate` の後に `evict` した hash が `object` で `None`、`reachable` が偽。
- [ ] 試験 `eviction_only_drops_readable_objects`：known にも `stored_objects` にも無いものは外れない。
- [ ] 試験 `required_objects_cover_every_read_of_an_edit`：読み取りを記録するストアで各編集を実行し、`required_objects` の対象から漏れた読み取りが無い（編集の種類ごとに1件）。
- [ ] 試験 `checkpoint_lists_evicted_objects`：外した後のチェックポイントの `checkpoint.json` に外したオブジェクトが残り、`stored_objects` から消えない。`check_history_store` と `package_bytes` の結果が外す前と同じ。
- [ ] ブラウザ試験 `save with history omit reads evicted objects back`：外した状態で `save(target, { history: "omit" })` が成功する。
- [ ] 試験 `save_keeps_evicted_objects_reachable`：外した後に同じ置き場へ `saveFolder` し、GC の dry-run が何も消さず、開き直すと同じ `snapshotHash`。

**Task STO-1c：読み出しの元と SDK。**

Files：Modify `packages/alto-sdk/src/storage-core.ts`（`readStored`、`loadCandidate` の選択肢、`collect` の除外）、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/document.ts`（`withObjects` の対象、読み直し）、`packages/alto-sdk/src/engine.ts`（`open` の `tiles`、`memory`）、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/errors.ts`（`source-changed`）、`packages/alto-sdk/api/alto-engine.api.md`。Consumes `packages/alto-sdk/src/zip-index.ts`（STO-5 で新規）。（新規）

- [ ] node 試験（`packages/alto-sdk/test/storage-core.test.js`）：`readStored returns pack slices and refuses unknown paths`、`collect keeps packs that hold evicted objects`。
- [ ] ブラウザ試験（`tests/e2e/image-version-tests.js`）：`pan loads tiles on demand`（予算 64 MiB、大きな文書を開いてパンし、未完成のフレームの後に全部ある場合と同じ画素）、`a tile arrives after the view moved`、`GPU loss with evicted tiles rebuilds`（`testing` の GPU 喪失の後、同じ画素）、`a replaced .alto source reports source-changed`。
- [ ] クラッシュ試験（`tests/e2e/crash-tests.js` に点 `evicted-open`（新規））：外した状態でタブを終わらせ、新しいタブで復旧して作業文書が一致する。QA-5 の表（13-verification）に1行足す。

**Task STO-1d：実メモリを超える文書での記録。**

- [ ] 判定条件の測定（B1〜B7、40分の長時間試験、予算3通り）を `doc/verification/` の新しい結果文書（新規）に記録し、`DEFAULT_RESIDENT_BYTES` を決める。
- [ ] `doc/MILESTONES.md`「上限・継続課題」6 の「作業文書のタイルを表示範囲に応じて読む段階は未実装」を直す。

完了条件：上の試験が通り、予算を超える文書でパン、ブラシ、Undo、保存、GPU 喪失、復旧が動く。検証：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`。

### 依存

- 先に要る項目：STO-5（`ZipIndex` を作る。`.alto` の範囲読みに使う）。QA-5（実装済み。クラッシュ試験の仕組み）。STO-9 は必須ではない（同じ `lazy.rs`、`gc.rs` を変えるので、先に入れると衝突が減る）。（新規）
- 先に入れる項目（同じファイル）：STO-10（`upload.rs` の目印と `TileStore::is_damaged`（新規）。段階2で先に入る）、VER-1（08。`ObjectTarget::Merge` を広げる。段階2）。本項目はその後に `wanted`、`pending`、`ObjectTarget` の `Nodes`、`Region` を足す（00-ROADMAP の重なりの表）。
- 後に続く項目：IMG-4（05。ミップの鍵はタイルの取得経路に依る）、STO-6（派生キャッシュは STO-1 の後に測る）、STO-10 の追加分（部分読み込みで読み戻した破損を damaged に入れる）、PERF-3（10。冷えた描画の測定の前提が変わる）。

## STO-2 文書別ジャーナル

### 現状

- 名前空間で既に分かれている。`createEngine({ storage: { namespace } })`（1〜128文字の `A-Z a-z 0-9 . _ - /`、既定 `default`）。名前空間ごとに別の OPFS フォルダ（`namespaceFolder`）と Web Locks（`alto-engine:<namespace>`）。使用中は `CONFLICT`（reason `namespace-in-use`）。
- 一覧と削除は既にある：`listRecoveryNamespaces()` が `{ namespace, bytes, modified }` の配列を返し、`deleteRecovery(namespace)` が消す（`packages/alto-sdk/src/recovery-store.ts`、`index.ts` から公開）。embed の試験が使う。
- 1エンジン1文書。別の文書を並行に編集するには、別の名前空間で別のエンジンを作る。これは今も動く。
- 足りないもの：名前空間を自動で作る手段、一覧の文書名（ホストが付ける見出し）と大きさ。`EngineCore.namespace` は `@internal`。
- `doc/MILESTONES.md`「上限・継続課題」7 は「オリジン当たり1作業領域」「文書別ジャーナル分離は未実装」のままで、名前空間を反映していない。

### 11-04 からの変化

- `recovery-store.ts` と `browser.ts` は変化なし。11-04 版の「`recovery-store.ts` の一覧から復旧を選べるようにする」は、一覧（`listRecoveryNamespaces`）が既にあるので、文書名を足す作業に縮めた。
- `storage-core.ts` は F-7（9755611、a2b7b00、0d5c90e）と D の試験（18c036a、f46062b）で変わった。名前空間のフォルダの形は同じ。

### 設計

- `createEngine({ storage: { namespace: "auto" } })`：`auto/<crypto.randomUUID()>` を名前空間にする。`auto/` の接頭辞は予約し、ホストが `auto/` で始まる名前を明示で渡すことは許す（復旧のため）。生成した名前は `engine.namespace`（公開に変える）で読める。ページを読み直すと別の名前になるので、復旧するホストは `listRecoveryNamespaces()` で選んで明示で渡す。これを API の説明に書く。
- 見出し：`engine.recovery.setLabel(label: string)`（新規）。1〜256文字の文字列、範囲外は `INVALID_ARGUMENT`。ページの側で名前空間のフォルダに `label.json`（新規。`{ "label": string, "width": number, "height": number, "updated": number }`）を書く。`width`、`height` は今の文書から、`updated` は書いた時刻（ms）。文書を開いた時とチェックポイントの後に書き直す。書けなくても編集とジャーナルを失敗にしない（警告もしない）。
- 一覧：`listRecoveryNamespaces()` の要素に `label?: string`、`width?: number`、`height?: number` を足す（`label.json` が読めて形が正しいときだけ）。
- `storage-core.ts` の `discard` は名前空間のフォルダごと消す（`removeEntry` の `recursive`）ので、`label.json` も消える。これを正しい振る舞いとする（見出しは捨てたジャーナルの説明なので一緒に消えてよい）。次に文書を開くかチェックポイントを書いたときに書き直す。起動時の片付けは pack と `objects/` だけを消すので `label.json` に触れない。`deleteRecovery` もフォルダごと消す。
- 1エンジンで複数の文書を開く案は採らない（Worker、GPU の予算、描画が1文書を前提にしている）。

### 形式と通信

- `format_version`、manifest：触れない。
- `PROTOCOL`：上げない。`label.json` はページの側で書き、Worker への要求は増えない。
- OPFS の名前空間のフォルダに `label.json` が増える。ジャーナルの形は変えない。

### 作業手順

Files：Modify `packages/alto-sdk/src/engine.ts`（`namespace: "auto"`、`namespace` の公開、`recovery.setLabel`）、`packages/alto-sdk/src/recovery-store.ts`（`label.json` の読み書き）、`packages/alto-sdk/api/alto-engine.api.md`、`doc/MILESTONES.md`（7 を名前空間の記述に直す）。

Interfaces：Produces `EngineOptions.storage.namespace: "auto" | string`、`Engine.namespace`、`Engine.recovery.setLabel`、`listRecoveryNamespaces()` の `label`、`width`、`height`。

- [ ] ブラウザ試験（`tests/e2e/embed-tests.js`）：`auto namespaces let two documents journal at once`（2つのエンジンを `auto` で作り、別々に描いて両方 `durable()`）、`recovery lists both with their labels`（タブを終わらせずに `dispose` の前の状態で一覧に2件、見出しと大きさが出る）、`a damaged label.json is ignored`。
- [ ] 完了条件：一覧から選んだ名前空間で復旧できる。`MILESTONES.md` 7 が今の仕組みと一致する。
- [ ] 検証：`pnpm --dir packages/alto-sdk build`、`pnpm --dir packages/alto-sdk api:check`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。
- 後に続く項目：STO-8（最近使ったファイルと復旧の一覧を同じ画面で使う場合）。工数は 11-04 版の M から S に見直した（00-ROADMAP）。

## STO-3 JPEG、TIFF 書き出し

### 現状

- `ExportImageOptions` は `type?: "image/png" | "image/webp"`、`scale?`、`region?` だけ（`packages/alto-sdk/src/document.ts`）。`quality` も背景色も無い。
- Worker の `exportImage` は `type` を `convertToBlob({ type })` に渡すので、型の外から `"image/jpeg"` を渡すとブラウザが JPEG にする（透明は黒になる）。型と試験は無い。
- TIFF の符号化器は無い。`alto-store` の依存は `zip`（既定の機能なし。Deflate を持たない）、`lz4_flex`、`sha2` など。`alto-wasm` は `alto-store` に直接は依存しない（`alto-engine` を通す）。
- 等倍の PNG は SDK の `encodePng` で、表示タイルの画素をそのまま符号化する（C の修正）。

### 11-04 からの変化

- 書き出しが「`convertToBlob` で PNG と WebP」から「等倍の PNG は `png.ts` で符号化、拡大縮小と WebP だけ canvas」に変わった（0634560）。JPEG も canvas を通るが、透明の合成は SDK で行う必要がある。
- TIFF は「`alto-store` に書き、Worker で実行」としていたが、64M 画素の RGBA（256 MiB）を WASM へ写して戻すと作業量の上限 512 MiB に届く。画素は WASM を通さず、Rust はヘッダーだけを作る形に変えた。
- 01-color（COL-1、COL-2、COL-3、COL-6）が同じ `ExportImageOptions` と `crates/alto-store/src/tiff.rs`（新規）を広げる設計になった（下の「依存」）。

### 設計

**SDK の型。**

```ts
export interface ExportImageOptions {
  type?: "image/png" | "image/webp" | "image/jpeg" | "image/tiff"; // jpeg、tiff は（新規）
  scale?: number;
  region?: Rect;
  /** jpeg と webp：0〜1、既定 0.92。ほかの形式で指定すると INVALID_ARGUMENT。 */
  quality?: number;            // （新規）
  /** jpeg：透明を合成する色（straight sRGB、各 0〜1）。既定 [1, 1, 1]。 */
  background?: [number, number, number]; // （新規）
  /** tiff：既定 "none"。 */
  compression?: "none" | "deflate";      // （新規）
}
```

- 検査は `document.ts` で行い、Worker へ渡す前に `INVALID_ARGUMENT`（reason `quality`、`background`、`compression`（新規の reason））。`quality` は NaN と範囲外を拒否。`background` は長さ3の有限の数で 0〜1。
- 01 の `depth`（COL-2）、`colorSpace`（COL-6）、`"image/x-exr"`（COL-3）と同じ型に並ぶ。組み合わせの検査（例：`depth: 16` と `image/jpeg` は `UNSUPPORTED`）は後から入る項目が足す。

**JPEG。** Worker の非乗算の画素を、`background` の上に straight で合成する：`c' = round(c × a / 255 + bg × 255 × (255 − a) / 255)`、`a' = 255`。整数で計算する（`bg` は `round(bg × 255)` にしてから）。その後 `OffscreenCanvas` に置き、`convertToBlob({ type: "image/jpeg", quality })`。拡大縮小は合成の後に今の canvas の経路で行う。結果の画素はブラウザの JPEG 符号化に依るので、試験は誤差で比べる。

**TIFF。** 画素は JS のまま、Rust はヘッダーだけを作る。

```rust
// crates/alto-store/src/tiff.rs（新規）
pub enum TiffCompression { None, Deflate }     // （新規）
pub struct TiffStrip { pub rows: u32, pub bytes: u32 } // （新規）
/// 8bit RGBA、straight alpha（ExtraSamples = 2）、PlanarConfiguration 1、リトルエンディアン。
/// ヘッダー、IFD、StripOffsets／StripByteCounts を返す。画素の帯は戻り値の後に順に置く。
pub fn rgba8_header(width: u32, height: u32, compression: TiffCompression, strips: &[TiffStrip])
    -> Result<Vec<u8>, TiffError>;            // （新規）
/// Display は今のエラーと同じ「CODE: detail」。alto-wasm はそのまま JS のエラーにする。
pub enum TiffError { InvalidArgument(String), ResourceLimit(String) } // （新規）
pub const MAX_TIFF_BYTES: u64 = (1 << 32) - 1; // （新規）オフセットが32ビットに収まる上限
```

- 帯の高さは 256 行（表示タイルの行と同じ）。無圧縮は各帯 `width × rows × 4` バイト。Deflate（Compression 8）は Worker が帯ごとに `CompressionStream("deflate")`（zlib の形。TIFF の Adobe Deflate と同じ）で圧縮し、長さを `rgba8_header` に渡す。Rust に Deflate の依存を足さない。（新規）
- タグ：ImageWidth、ImageLength、BitsPerSample（8,8,8,8）、Compression（1 か 8）、PhotometricInterpretation 2、StripOffsets、SamplesPerPixel 4、RowsPerStrip 256、StripByteCounts、PlanarConfiguration 1、ExtraSamples 2、SampleFormat 1。タグは番号の昇順。
- エラー：`StoreError` には `UNSUPPORTED`、`CORRUPT_DATA`、`STORAGE` しか無いので、TIFF は専用の `TiffError`（新規）を持つ。幅か高さが 0、帯の行の合計が高さと違う、帯の長さが無圧縮の値と違うものは `INVALID_ARGUMENT`。帯の合計とヘッダーが `MAX_TIFF_BYTES` を超えたら `RESOURCE_LIMIT`。64M 画素の無圧縮（256 MiB）は収まる。（新規）
- `rgba8_header` は `alto-wasm` の新しい関数 `tiffHeader`（新規）から呼ぶ。`alto-wasm` に `alto-store` の依存（同じリポジトリの crate）を足す。新しい外部の依存は足さない。
- 出力は `new Blob([header, ...strips], { type: "image/tiff" })`。画素の写しは Blob の組み立ての1回だけで、作業量の見積りは今の式に収まる。

**上限。** 今の 64M 画素と 512 MiB の作業量を変えない。JPEG は今の式に合成の分（段の画素 × 4）を足して見積る。

### 形式と通信

- `format_version`、manifest：触れない。
- `PROTOCOL`：9 に含める（段階1）。`exportImage` の引数に `quality`、`background`、`compression` が増える。`tiffHeader` は WASM の関数で、Worker の要求ではない。（新規）
- 描画の版：触れない。

### 作業手順

Files：Create `crates/alto-store/src/tiff.rs`（新規）。Modify `crates/alto-store/src/lib.rs`（`pub mod tiff`）、`crates/alto-wasm/Cargo.toml`（`alto-store` の依存）、`crates/alto-wasm/src/lib.rs`（`tiffHeader`）、`packages/alto-sdk/src/document.ts`（型と検査）、`packages/alto-sdk/src/worker.ts`（JPEG の合成、TIFF の組み立て）、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/errors.ts`（reason）、`packages/alto-sdk/api/alto-engine.api.md`、`doc/design/EXPORT-CONTRACT.md`（JPEG と TIFF の契約）。

Interfaces：Produces `alto_store::tiff::rgba8_header`、`TiffCompression`、`TiffStrip`、`TiffError`、`exportImage({ type: "image/jpeg" | "image/tiff", quality, background, compression })`。

- [ ] Rust 試験（`tiff.rs`）：`tiff_header_fields_are_valid`（タグの値と昇順、オフセットの和）、`tiff_strip_rows_must_cover_the_height`（帯の行の合計が高さと違うと `INVALID_ARGUMENT`）、`tiff_over_4_gib_is_refused`。
- [ ] node 試験（`packages/alto-sdk/test/`、新規 `tiff.test.js`）：ヘッダーと帯を自作の最小の TIFF 読み取りで読み、画素が入力と一致する（無圧縮と Deflate）。
- [ ] ブラウザ試験（`tests/e2e/embed-tests.js`）：`jpeg export composites alpha over the background`（半透明の赤を白と黒の背景で書き出し、`createImageBitmap` で読んで各チャネル ±3）、`quality outside 0..1 is invalid`、`tiff export holds the display pixels`（等倍の TIFF の画素が PNG の書き出しと全画素一致）、`quality on png is invalid`。
- [ ] 完了条件：上の試験。既存の PNG、WebP の書き出しの試験が変わらず通る。`EXPORT-CONTRACT.md` に JPEG（非可逆、背景の合成）と TIFF（straight、ExtraSamples 2）を書く。
- [ ] 検証：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。
- 後に続く項目：COL-6（01。`tiff.rs` に CMYK とプロファイルの埋め込みを足す。画素が WASM を通る見積りは 01 で直す）、COL-1、COL-2、COL-3（01。同じ `ExportImageOptions` を広げる。型と `tiff.rs` の持ち主は本項目で、段階1で先に入る（00-ROADMAP の重なりの表））、STO-4（逐次の経路を PNG から TIFF に広げる場合）。`ExportImageOptions` は STO-3 を先に入れ、01 の項目が後から足す順にする（00-ROADMAP の重なりの表で確定）。

## STO-4 逐次エンコーダー

### 現状

- `exportImage` は段の画素全体を `Uint8ClampedArray` に集めてから符号化する。上限は段の画素で 64M 画素と作業量 512 MiB（共通の現状）。
- `encodePng` は全行に Paeth のフィルタを掛けた配列を1つ作り、`CompressionStream("deflate")` に通す。IDAT は1つ。
- 文書の辺の上限は 65536（`doc/MILESTONES.md`「上限・継続課題」3）。64M 画素を超える文書は全体を等倍で書き出せない。

### 11-04 からの変化

- PNG の符号化が SDK の `png.ts` に移った（0634560）。11-04 版の「行フィルタ（なし）」は今の符号化（Paeth）と違う。逐次でも Paeth を使い、一括の出力と同じ画素になるようにした（バイト列は IDAT の分け方で変わる）。

### 設計

- `doc.exportImageStream(options: ExportImageOptions): ReadableStream<Uint8Array>`（新規）。対象は「`type` が PNG で、縮小なしか段の画素と同じ大きさ」（今の `exact` の条件）だけ。ほかは `UNSUPPORTED`（reason なし、メッセージで条件を示す）。
- 段の画素の上限 64M を外し、文書の辺（65536）までにする。作業量は同時に持つ帯の数で決まる：`MAX_STREAM_BANDS = 2`（新規）、1帯 = 256 行。1帯の画素 `w × 256 × 4` とフィルタ後の `w × 256 × 4 + 256` と前の行 `w × 4`。辺 65536 で約 128 MiB。`readbackCapacity` の読み戻しを足して 512 MiB の見積りに収める。
- Worker の要求（pull 型）：
  - `exportStreamOpen { region, scale }` → `{ id }`（新規）
  - `exportStreamPull { id }` → `Uint8Array | null`（新規。次の PNG のチャンク列。`null` で終わり）
  - `exportStreamClose { id }`（新規。途中でやめる）
- 同時に開ける数は1（`BUSY` で拒否）。Worker の再起動、文書の差し替え、`sceneGeneration` の変化（編集）では、次の `pull` が `CONFLICT`（reason `document-changed`（新規））で失敗し、読み戻しの資源を放す。編集を止めて待つ案は採らない（読まない利用者が編集を止められるため）。
- 符号化：Worker が `CompressionStream("deflate")` を1本持ち、帯ごとに Paeth を掛けた行を書き込み、出てきた圧縮バイトを `IDAT` チャンク（1チャンク最大 1 MiB）にして返す。最初の `pull` は署名、`IHDR`、`sRGB`。最後は `IEND`。前の帯の最後の行を次の帯の Paeth の上の行に使う。
- SDK は `new ReadableStream({ pull, cancel })` で包む。`cancel` で `exportStreamClose`。`exportImage` の一括の経路は今のまま残す。（新規）
- WebP と JPEG は canvas が全体を要るので対象外（今のまま）。TIFF の逐次は、帯の長さを先に知る必要があるので、無圧縮だけなら可能。本項目には含めない（候補として記録）。

### 形式と通信

- `format_version`、manifest：触れない。
- `PROTOCOL`：上げる（10。段階2の回）。要求3つ。
- 描画の版：触れない。

### 作業手順

Files：Modify `packages/alto-sdk/src/png.ts`（逐次の符号化器 `PngStreamEncoder`（新規））、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/errors.ts`（`document-changed`）、`packages/alto-sdk/api/alto-engine.api.md`、`doc/design/EXPORT-CONTRACT.md`。

Interfaces：Produces `DocumentHandle.exportImageStream`、`PngStreamEncoder`。

- [ ] node 試験（`packages/alto-sdk/test/png.test.js`）：`stream encoder output decodes to the same pixels as encodePng`（帯の境目をまたぐ Paeth を含む）、`a stream split into many IDAT chunks is valid`（CRC と長さ）。
- [ ] ブラウザ試験（`tests/e2e/embed-tests.js`）：`stream export matches the one shot PNG pixels`、`a cancelled stream releases its readback`（途中で `cancel` し、次の `exportImage` が通る）、`an edit during a stream fails it with document-changed`、`a stream larger than 64M pixels is allowed`（SwiftShader で時間がかかるなら Metal だけで実行し、記録する）。
- [ ] 完了条件：上の試験。一括の経路の試験が変わらない。
- [ ] 検証：`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし（STO-3 の型の後に入れると `ExportImageOptions` の衝突が減る）。
- 後に続く項目：COL-2（01。16bit の PNG を逐次にする場合）。COL-1、COL-5（01。`png.ts` に cICP、iCCP を足す。本項目の `PngStreamEncoder`（新規）の後に入る（00-ROADMAP の重なりの表））。

## STO-5 保存ファイルへのプレビュー格納

### 現状

- `.alto` にプレビューは無い。ファイル一覧に縮小画像を出すには、文書を開いて書き出すしかない。
- `Manifest` は `deny_unknown_fields` で、読み手は format 2／3 だけを受ける。manifest に欄を足すと、今の読み手は開けない。
- 読み手は manifest、`snapshots/`、`commits/`、`objects/`、`external-objects.json` 以外のエントリを無視する（`decode_entries`、`openFolder` の読み込み）。
- `.alto` を部分的に読む仕組み（中央ディレクトリだけを読む）は無い。TS に ZIP の読み手は無い。
- フォルダ形式には、比較用の `document.json`、`.gitattributes`、`.gitignore` という manifest の外のファイルが既にある（`folder_entries`）。

### 11-04 からの変化

- manifest format 3（薄い `.alto`、8c6d2fd）が入った。11-04 版の「manifest に `preview: { path, hash, snapshot }` を記録する」は、`deny_unknown_fields` の manifest に欄を足すので、format を上げる必要がある。別のエントリに分けて、format を上げない形に変えた。

### 設計

- エントリを2つ足す。どちらも manifest の外で、無くても何も壊れない。
  - `preview.png`：表示のピラミッドから、長辺が 512 以下になる最初の段の画素を `encodePng` で符号化したもの。
  - `preview.json`：`{ "snapshot": "<manifest.snapshot と同じ16進>", "width": number, "height": number, "sha256": "<preview.png の SHA-256 の16進>" }`。
- 生成：`save(target, { preview: true })`（新規の選択肢）。既定は案 A／B の決定による（下）。Worker が保存の要求の前に、`exportImage` と同じ読み戻しで段の画素を読み、`encodePng` で作る。保存の要求に `preview?: ArrayBuffer` を足す。
- Rust：`PackageParts.extra: BTreeMap<String, Vec<u8>>`（新規）。書くとき（`PackageParts.extra` を組み立てるとき）に受ける名前は `preview.png` と `preview.json` だけで、ほかは `INVALID_ARGUMENT`。読むときは今の規則のまま、未知のエントリを ZIP とパスの検査（`read_entries`）の後に無視する（`decode_entries` は認めたパスだけを使う）。プレビューを持つパッケージは今の読み手でも開ける。`package_sources` が加え、上限 512 MiB の合計に数える。`MAX_PREVIEW_BYTES = 4 MiB`（新規）を超えると、プレビューを入れずに保存する（保存は失敗させない）。`preview.json` の `snapshot` は Rust が保存するスナップショットのハッシュで上書きする（Worker の値を信じない）。
- `packageSize` はプレビューを含めない（作る前に大きさが分からない）。説明に書く。
- 薄い `.alto`：同じく入れる。フォルダ形式：`preview.png`、`preview.json` を `writeRef` で書く（`document.json` と同じ、内容アドレスでないファイル）。書けなくても保存は失敗にしない（`document.json` の扱いと同じく警告）。
- 読み手：`readPreview(source: Blob | ArchiveSource): Promise<Blob | null>`（新規。`@alto/engine/browser`）。エンジンを起動せずに読む。
  - `ZipIndex`（`packages/alto-sdk/src/zip-index.ts`（新規））で末尾から中央ディレクトリを読み、`manifest.json`、`preview.json`、`preview.png` の3つだけを `Blob.slice` で読む。
  - `preview.json` の `snapshot` が manifest の `snapshot` と一致し、`sha256` が `preview.png` と一致するときだけ返す。ほかは `null`（例外にしない）。
- `ZipIndex` の規則：EOCD を末尾 65,557 バイトから探す。Zip64 の EOCD と locator を読む（エントリが 65,535 を超えると `encode_parts_fetching` が Zip64 にする）。全エントリが Stored（method 0）で、圧縮後と圧縮前の長さが同じ。今の読み手（`read_entries`）は方式を `ZipArchive` に任せているが、`zip` は既定の機能なし（`crates/alto-store/Cargo.toml`）で Deflate を持たないので、実際に読めるのは Stored だけで、書き手も Stored だけを書く。`ZipIndex` はこの事実上の境界を明文化するもので、今の読み手が受ける `.alto` を拒まない。`zip` に Deflate の機能を足す変更が入ったら、`ZipIndex` は Stored 以外のエントリを「範囲読みできない」として全体読み（今の `open`）へ戻す（拒否しない）。名前は `..` と先頭の `/` を含まず、重複しない。ローカルヘッダーの位置と長さがファイルの中に収まり、重ならない。上限：エントリ数 1,000,000（`read_entries` と同じ）、中央ディレクトリ 64 MiB（`MAX_CENTRAL_DIRECTORY_BYTES`（新規））、名前 1024 バイト。違反は `CORRUPT_DATA`。STO-1 の範囲読みも同じ `ZipIndex` を使う。

**利用者の決定が要る点。** 既定でプレビューを入れるか。

| 案 | 内容 | 影響 |
|---|---|---|
| A | 既定で入れる（`preview: false` で外す） | 保存のたびに読み戻しと符号化が増える。`.alto` のバイト列が `CompressionStream` の実装に依る |
| B（既定） | `preview: true` のときだけ入れる | 今の保存のバイト列と時間を変えない |

### 形式と通信

- manifest の `format`：上げない（manifest を変えない）。`format_version`：触れない。今の読み手は新しいエントリを無視する。
- `PROTOCOL`：上げる（10。段階2の回）。`save` の要求の `preview`。
- 描画の版：触れない。

### 作業手順

Files：Create `packages/alto-sdk/src/zip-index.ts`（新規）。Modify `crates/alto-store/src/lib.rs`（`PackageParts.extra`、`package_sources`、`folder_entries`）、`crates/alto-engine/src/versions.rs`（保存の部品にプレビューを渡す）、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/document.ts`（`save` の `preview`）、`packages/alto-sdk/src/browser.ts`（`readPreview`）、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`packages/alto-sdk/api/alto-engine-browser.api.md`。

Interfaces：Produces `ZipIndex.read(blob): Promise<ZipIndex>`、`ZipIndex.entry(name)`、`readPreview`、`save(target, { preview })`、`PackageParts.extra`。

- [ ] Rust 試験（`crates/alto-store/src/lib.rs`）：`preview_entries_count_toward_the_cap`、`extra_entry_other_than_preview_is_refused_on_write`（書き手の検査）、`unknown_entry_is_ignored_on_read`（未知のエントリを持つ `.alto` を今の読み手と同じく開ける）、`a_package_with_a_preview_decodes_as_before`（`decode_entries` の結果がプレビューの有無で同じ）。
- [ ] node 試験（`packages/alto-sdk/test/zip-index.test.js`（新規））：`zip index reads one entry without the rest`、`truncated central directory is corrupt`、`zip64 end record is read`、`overlapping entries are corrupt`。変異試験（先頭・長さ・1ビット反転・切り詰め）で例外の型が `AltoError` の `CORRUPT_DATA` だけになる（13 の QA-1c と同じ作り）。
- [ ] ブラウザ試験（`tests/e2e/image-version-tests.js`）：`a stale preview is ignored`（`preview.json` の snapshot を書き換えた `.alto` で `null`）、`a package without a preview opens`、`readPreview returns the 512 px image`（長辺 512 以下、表示の段の画素と一致）、`a thin .alto keeps its preview`。
- [ ] 完了条件：上の試験。プレビューの無い保存のバイト列が変わらない。
- [ ] 検証：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`pnpm --dir packages/alto-sdk api:check`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。
- 後に続く項目：STO-1（`ZipIndex` を範囲読みに使う）、STO-8（最近使ったファイルの一覧にプレビューを出す場合）。QA-1（13）の規則により、`ZipIndex` は外部バイト列の新しい入口なので、同じ PR で変異試験を足す（cargo-fuzz の target は TS に効かないため）。（新規）

## STO-6 派生 OPFS キャッシュ

### 現状

- 完成した表示タイルは GPU の表示キャッシュにだけある。Worker の再起動、GPU 喪失、開き直しで失われ、作り直す。
- 復旧の OPFS（名前空間のフォルダ）は正本のジャーナルで、派生物は置いていない。
- 設計は [COLD-RENDERING-DESIGN.md](../COLD-RENDERING-DESIGN.md) の「C. 再起動・再オープン向けの破棄可能な派生キャッシュ」にある。採否は C4（復旧コールド）で命中時の短縮を測ってから決める、という条件が付いている。測定はまだ無い。

### 11-04 からの変化

- 変化なし（派生キャッシュのコードは無い）。表示の読み戻しは書き出しが使う `readTiles`（`crates/alto-gpu/src/renderer/tile_read.rs`）があり、725d6f4 で `renderer.rs` から分かれた。
- 05-image-geometry（IMG-4）が、ミップの保管場所を STO-6 と同じにするかを STO-1／STO-6 の後に決める、と書いた。

### 設計

COLD-RENDERING-DESIGN の C をそのまま採る。ここでは今のコードに合わせた点だけを書く。

- 鍵：`<snapshot の内容ハッシュ>/<描画の経路>/<level>/<x>/<y>`。描画の経路は render v3 の識別と、01 の `RenderPath`（新規）（COL の項目が入れる）の値。表示の色空間（COL-1）が入ったら、それも鍵に入れる。編集のたびに、その snapshot の項目は全部不適格（領域ごとの無効化は後続）。
- 中身：1項目 = ヘッダー（形式 1、鍵、長さ、BLAKE3 ではなく SHA-256 の16進）＋ premultiplied RGBA8 の 262,144 バイト。読むときに長さとハッシュを検査してから GPU に上げる。
- 置き場：OPFS の `alto-cache/`（新規。名前空間のフォルダの外）。容量は `navigator.storage.estimate()` の残りの 10% と 256 MiB の小さい方（`MAX_DERIVED_CACHE_BYTES`（新規））。超えたら古い項目から消す。書き込みの失敗、容量不足、破損は、キャッシュの無効として黙って扱い、保存や編集の失敗にしない。
- 専用の Cache Worker（`packages/alto-sdk/src/cache-worker.ts`（新規））。Engine Worker と Storage Worker を止めない。読み戻しは同時1枚。
- 書く時：編集の入力が無い時間（`AUTO_CHECKPOINT_IDLE_MS` と同じ 2 秒）に、可視の表示タイルと隣接する低倍率の親だけ。読み込みは、復旧と開き直しの最初のフレームで、可視の表示タイルの鍵を Cache Worker に尋ね、届いたものを表示キャッシュに入れる。届く前に合成が終わればキャッシュの結果を捨てる。
- 採否の判定（測定で形が変わる）：PERF-5（10）の段階別の計時を入れた後、C4（Worker 再起動とジャーナル復旧）を、キャッシュあり・なしで各5回測る。命中時の「全可視 GPU 完了」までの中央値が 20% 以上短くなり、B1 の p95 と GPU の計上値が悪くならない場合だけ採る。満たさなければ実装を戻し、結果だけを記録する。

### 形式と通信

- `format_version`、manifest：触れない。保存ファイルに何も足さない。
- `PROTOCOL`：Engine Worker への要求は増えない見込み（Cache Worker は Engine Worker が起こす）。ページから有効・無効を切り替える選択肢 `createEngine({ cache: { derived: boolean } })`（新規）を足すなら上げる（11。段階3の回）。
- 描画の版：触れない。キャッシュの画素は合成結果の写しで、鍵に描画の経路を含める。

### 作業手順

- [ ] 前提：PERF-5（10）の計時が入っていること。STO-1 の後に測る（部分読み込みで冷えた描画の内訳が変わるため）。
- [ ] 試作：Create `packages/alto-sdk/src/cache-worker.ts`（新規）。Modify `packages/alto-sdk/src/worker.ts`、`crates/alto-wasm/src/lib.rs`（完成した表示タイルを表示キャッシュへ入れる入口 `installDisplayTile`（新規））、`crates/alto-gpu/src/renderer/cache.rs`。
- [ ] node 試験（`packages/alto-sdk/test/derived-cache.test.js`（新規））：`a damaged entry is ignored`、`entries over the budget are evicted oldest first`、`a write failure does not surface`。
- [ ] ブラウザ試験（`tests/e2e/image-version-tests.js`）：`a cached tile equals the composed tile`（キャッシュから入れた表示タイルと合成した表示タイルが全バイト一致）、`an edit makes the cached entries ineligible`。
- [ ] 判定の測定を `doc/verification/` の結果文書に記録する。採らない場合は試作を消し、記録だけを残す。
- [ ] 検証：`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`node scripts/verify-browser.mjs --suite=perf`。

### 依存

- 先に要る項目：PERF-5（10。計時）、STO-1（測定の前提）。
- 後に続く項目：PERF-3（10。冷えた描画の改善の候補の1つ）、IMG-4（05。保管場所を共有するか）。

## STO-7 ステージング退避、未確定ストロークの復元

### 現状

- ストローク中の入力（Worker の `input` → `Runtime::brush_input`）はジャーナルに書かない。`endBrush` の確定で、読み戻した結果の Transaction を1件書く（`queueJournal`）。タブが落ちると、確定前のストロークは失われる。
- クラッシュ試験の `stroke-open`（`tests/e2e/crash-tests.js`、`crash-page.html`）は「直前の durable な状態に戻る」ことを確かめている（D-1）。13-verification の QA-5 の表の1行目も同じ期待。
- ブラシは GPU の f32 のスタンプと読み戻しで確定する（`renderer/stroke.rs`）。CPU で同じ結果を作る経路は無い。`alto_raster::cpu_paint` は試験用の参照で、GPU と全バイトは一致しない。
- 入力の座標は `Runtime` の `input_inverse` で文書の座標に直してから `StrokeSampler` に入る。ストロークの設定は `beginBrush` の `layer`、`config`、`color`、`erase`、`mask`。
- ジャーナルの記録は ZIP で、`transaction.json`（`kind` が `transact`／`undo`／`redo`）か `version-state.json`（版の操作）を持つ。`Engine::replay_record` が読む。

### 11-04 からの変化

- 11-04 版の「CPU のスタンプ（`stroke_q8`）で確定する」は誤り。`stroke_q8`（`crates/alto-gpu/src/stroke_q8.rs`）はベクターの線の輪郭で、ブラシではない。CPU で GPU と同じ画素を作る経路は無いので、復旧は GPU のブラシで描き直す形に変えた。
- D-1（867b19b）で `stroke-open` の試験が入った。本項目はその期待を変える（13-verification の QA-5 が、09 の側で表を直すよう求めている）。
- `worker.ts` は D-7 の修正（850063c）と F-7（9755611、a2b7b00）で変わったが、`input` と `endBrush` のジャーナルの扱いは同じ。

### 設計

**1. 記録。** ジャーナルの記録に `stroke.json`（新規）を持つ種類を足す。

```json
{ "stroke": "<uuid>", "seq": 0, "begin": { "layer": "...", "config": {...}, "color": [r,g,b,a], "erase": false, "mask": false } | null,
  "samples": [x, y, pressure, tiltX, tiltY, tMs, ...] }
```

- `samples` は `brush_input` が受けたままの値（`input_inverse` を掛ける前）。`begin_brush` はレイヤーの変形から `input_inverse` を毎回作り直し、`brush_input` と選択の変換（`stamp`）がそれを使う。復旧で描き直すときの文書は `beginBrush` の時点と同じ（3.）なので同じ `input_inverse` が作られ、変換を2回掛けない。`begin` は `seq` 0 だけが持つ。設定（`config`）は `beginBrush` に渡った JSON をそのまま入れる（BR-1〜BR-5 の欄が増えても記録の形は変わらない）。
- 書く時：Worker が入力を `STROKE_JOURNAL_MS = 250`（新規）ごと、または 512 サンプルごとにまとめて `enqueue({ kind: "append" })` する。`durable()` はこれも待つ。
- 閉じる：`endBrush` の Transaction 記録の `transaction.json` に `"stroke": "<uuid>"` を足す。`cancelBrush` は `stroke.json` に `"cancel": true` の記録を書く。
- 上限：1ストローク `MAX_JOURNALED_STROKE_SAMPLES = 65536`（新規）。超えた分は記録せず、最後の記録に `"truncated": true` を書く（復旧は記録した分だけ描く）。チェックポイントは開いたストロークの記録を消さない（チェックポイントの後の追記として残す。自動チェックポイントはストローク中に走らない今の条件を確かめる）。

**2. 復旧。**

- `Engine::replay_record` は `stroke.json` を読んだら、文書を変えずに `Engine` の外（`Runtime`）へ返す（`Replayed::Stroke`（新規））。`Runtime::replay_records` は、閉じた（Transaction か cancel が後に来た）ストロークを捨て、最後に開いたままのストロークだけを残す。
- 全記録の再生の後、開いたストロークがあれば、`Runtime::replay_open_stroke()`（新規、async）が `begin_brush` → `brush_input`（記録した値をそのまま渡す。新しい WASM の入口は要らない）→ `end_brush` を実行する。結果は新しい Transaction（intent `recovered.stroke`（新規））としてジャーナルに書く。Undo で戻せる。
- 復旧の結果に警告 `RECOVERED_STROKE`（新規）を `WarningCode` に足して付け、`details.samples` に描いた数、`details.truncated` を入れる。
- 当時の画面と同じ画素である保証は無い（GPU の f32 の違い、筆圧の時刻の扱い）。同じ GPU では同じ入力から同じ画素になるはずだが、試験は構造で確かめる（下）。
- 復旧で描き直さない場合：対象のレイヤーが無い（`NOT_FOUND`）、設定が今の SDK で無効、BR-2／BR-3 の画像の先端・テクスチャが文書の外（SDK から渡す形）にある（03 の決定による）。このときは描かずに警告 `RECOVERED_STROKE`（`details.skipped` に理由）を出す。復旧そのものは失敗にしない。（新規）

**3. 他の処理との関係。**

- ストローク中は他の編集が `tool-active` で拒まれるので、復旧で描き直すときの文書は `beginBrush` の時点と同じ。
- STO-1 と組み合わせる場合、描き直しの前にレイヤーのタイルを読む（STO-1 のストロークの pin と同じ）。
- 薄い `.alto`（F-9）の文書でも同じ。版のタイルは使わない。

### 形式と通信

- `format_version`、manifest：触れない。
- ジャーナル：記録の種類が増える。今の SDK は `stroke.json` だけの記録を `journal transaction missing`（`CORRUPT_DATA`）で拒むので、古い SDK で新しいジャーナルを復旧できない。リリース前なので移行は作らない（共通規則3）。復旧の候補を古い世代へ戻す今の仕組みで扱われることを確かめる。
- `PROTOCOL`：上げない見込み（ページと Worker の要求は変わらない）。`WarningCode` の追加は公開の型の変更として API 報告を更新する。
- 描画の版：触れない。

### 作業手順

Files：Modify `crates/alto-engine/src/lib.rs`（`replay_record` の分岐、`Replayed::Stroke`）、`crates/alto-wasm/src/lib.rs`（記録の組み立て、`replay_records`、`replay_open_stroke`）、`packages/alto-sdk/src/worker.ts`（`input` のまとめ書き、`cancelBrush` の記録、復旧後の描き直し）、`packages/alto-sdk/src/types.ts`（`RECOVERED_STROKE`）、`packages/alto-sdk/api/alto-engine.api.md`、`tests/e2e/crash-tests.js`、`tests/e2e/crash-page.html`、`doc/design/backlog/13-verification.md`（QA-5 の表の1行目）、`doc/verification/ENGINE-HARDENING-RESULTS.md`（D-1 の期待が変わったことの追記）。

Interfaces：Produces 記録 `stroke.json`、`Replayed::Stroke`、`Runtime::replay_open_stroke`、警告 `RECOVERED_STROKE`。

- [ ] Rust 試験（`crates/alto-engine/src/lib.rs`）：`stroke_records_do_not_change_the_document_on_replay`、`a_closed_stroke_is_not_replayed`（Transaction で閉じたストロークと cancel したストロークは返らない）。
- [ ] ブラウザ試験（`tests/e2e/image-version-tests.js`）：`a recovered stroke is one undoable transaction`（`restartWorker` の試験用の入口で再起動し、Undo で直前の状態と一致）。
- [ ] クラッシュ試験：`stroke-open` の期待を変える。新しい期待は「(1) 復旧後の Undo の深さが1つ増え、(2) Undo すると直前の durable な状態（今の期待）と snapshot hash が一致し、(3) 対象のレイヤーのタイルが、記録したサンプルの外接矩形の外で変わらず、内側で少なくとも1枚変わる、(4) 警告 `RECOVERED_STROKE` が1件」。Metal と SwiftShader で `ALTO_CRASH_ONLY=stroke-open ALTO_CRASH_REPEAT=10`。
- [ ] クラッシュ試験 `stroke-open-transformed`（新規）：回転と拡縮を持つレイヤー（親のグループにも変形）で同じ期待。描き直したタイルの変化が、記録したサンプルを `input_inverse` で写した外接矩形の内側だけにある（変換を2回掛けると外れる）。
- [ ] クラッシュ試験に `stroke-open-textured`（新規）：画像の先端・テクスチャを SDK から渡す形（03 の決定が「SDK から渡す」の場合）のストロークは描き直さず、直前の durable な状態と `details.skipped`。
- [ ] 13-verification の QA-5 の表の1行目の期待を「確定前のストロークは、記録した入力から描き直して1つの Transaction として戻る（画素は当時の画面と同一でない）」に直す。
- [ ] 完了条件：上の試験。`durable()` を待ったストロークの入力が復旧で失われない。
- [ ] 検証：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`、`ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`。

### 依存

- 先に要る項目：QA-5（実装済み）。BR-1〜BR-5（03）の設定の欄は記録の形に影響しないが、BR-2／BR-3 の先端の置き場（03 の利用者の決定）で描き直せる範囲が決まる。
- 後に続く項目：QA-5 の残り（13。`append-random-kill` の判定に、開いたストロークの描き直しを含める）。

## STO-8 最近使ったファイル

### 現状

- `@alto/engine/browser`（`packages/alto-sdk/src/browser.ts`）は、`fileHandleTarget`、`downloadTarget`、`blobTarget`、`pickSaveTarget`、`fileSource`、`pickFolder`、`opfsFolder`、`isSupported` を持つ。ハンドルを覚える仕組みは無い。
- 薄い `.alto`（F-9）は、開くときに版の置き場（`history`）を別に渡す必要がある。ファイルと置き場の組を覚える手段は無い。

### 11-04 からの変化

- `browser.ts` は変化なし。F-9 で「`.alto` と版の置き場の組」が生まれたので、覚える対象に置き場のハンドルを足した。

### 設計

- IndexedDB のデータベース `alto-engine-recent`（新規）、ストア `files`。要素は `{ id: string, name: string, kind: "file" | "folder", handle: FileSystemHandle, history?: FileSystemDirectoryHandle, opened: number }`。`id` は `crypto.randomUUID()`。
- API（`@alto/engine/browser`、すべて新規）：
  - `rememberFile(handle: FileSystemFileHandle | FileSystemDirectoryHandle, options?: { history?: FileSystemDirectoryHandle }): Promise<RecentFile>`。同じハンドル（ブラウザの isSameEntry）は1件にまとめ、`opened` を更新する。
  - `recentFiles(): Promise<RecentFile[]>`：新しい順。権限は確かめない（一覧に出すだけ）。
  - `openRecent(id): Promise<{ kind: "file", source: ArchiveSource, history?: ObjectStorageProvider } | { kind: "folder", source: ObjectStorageProvider }>`：`kind` で `engine.open` か `engine.openFolder` かを選べるようにする。ファイルは `fileSource(handle)`、フォルダと `history` は `FileSystemDirectoryProvider`（`pickFolder` と同じ包み方）で包んで返す。ブラウザの queryPermission、なければ requestPermission（readwrite）（利用者の操作の中で呼ぶ必要がある、と説明に書く）。拒否は `STORAGE`（reason なし、メッセージ `permission denied`）で、要素は消さない。
  - `forgetFile(id): Promise<void>`。
- 上限：`MAX_RECENT_FILES = 32`（新規）。超えたら古いものから消す。
- 本体（`@alto/engine`）には入れない。外部への送信は無い。IndexedDB が使えない環境では `recentFiles` が空、`rememberFile` が `UNSUPPORTED`。（新規）

### 形式と通信

- `format_version`、manifest、`PROTOCOL`：触れない（ページの側だけ）。
- `@alto/engine/browser` の API 報告（`packages/alto-sdk/api/alto-engine-browser.api.md`）を更新する。

### 作業手順

Files：Create `packages/alto-sdk/src/recent-files.ts`（新規）。Modify `packages/alto-sdk/src/browser.ts`（再公開）、`packages/alto-sdk/api/alto-engine-browser.api.md`。

Interfaces：Produces `rememberFile`、`recentFiles`、`openRecent`、`forgetFile`、`RecentFile`。

- [ ] ブラウザ試験（`tests/e2e/embed-tests.js`。File System Access のハンドルの代わりに OPFS のハンドルで試す）：`recent files keep the newest 32`、`the same file is remembered once`、`a revoked permission reports without removing the entry`（`queryPermission` を差し替えて試す）、`a thin .alto is remembered with its history folder`。
- [ ] 完了条件：上の試験。`@alto/engine` の本体の API が変わらない。
- [ ] 検証：`pnpm --dir packages/alto-sdk build`、`pnpm --dir packages/alto-sdk api:check`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。
- 後に続く項目：STO-5（一覧にプレビューを出す場合、`readPreview` を組み合わせる。依存ではない）。（新規）

## STO-9 タイル表の別オブジェクト化

### 現状

- タイル表は `alto_raster::TileMap { format, tiles: BTreeMap<TileKey, ContentHash> }`（`deny_unknown_fields`）。ラスターの内容、マスク、選択、効果の選択範囲が持ち、スナップショットの JSON（`Document::snapshot_bytes`、`crates/alto-doc/src/document.rs`）にそのまま入る。
- 版ごとにスナップショットを1つ持つ。薄い `.alto` は全ての版のスナップショットを持つ（F-9）。
- F-9 の4時間試験で、薄い `.alto` は版1つあたり約 0.33 MiB 増えた（最後の1時間、タイル数 850）。原因は「全ての版のスナップショット・commit と外に置いたオブジェクトの一覧」と考えられるが、内訳は測っていない。
- GC の根は `doc_hashes`（`crates/alto-engine/src/versions.rs`）と `effect_hashes`（`crates/alto-engine/src/selection.rs`）。差分は `alto_doc::diff` が `DiffItem` を返し、画素を読まない。
- 文書の形式は `format_version` 5（`DocumentMeta`、`crates/alto-doc/src/lib.rs`）、manifest の `document_format` も 5。

### 11-04 からの変化

- alto-doc の `lib.rs` は 126f2b5 で分かれ、スナップショットの読み書きは `crates/alto-doc/src/document.rs` にある。11-04 版の作業手順の `crates/alto-doc/src/lib.rs` はこちらに読み替える。
- F-9 で、スナップショットは薄い `.alto` に必ず入り、log・diff・status は置き場なしでも動く約束になった。タイル表を `objects/` に置くと、置き場が無いときに版のスナップショットを読めなくなる。置き場所を `objects/` から分けた（下）。
- `gc.rs` は 8c6d2fd（薄い `.alto`）で変わった。
- 11-04 版の「設計書 4.1 の案」の出典はリポジトリに無い。動機を F-9 の計測に置き直し、測ってから採る判定条件を付けた。

### 設計

**判定条件（先に測る）。** F-9 と同じ長時間試験（`ALTO_SOAK_THIN=1`、seed 1）の 120分の薄い `.alto` で、エントリの種類ごとの大きさ（スナップショット、commit、`external-objects.json`、作業文書のオブジェクト）を測る。スナップショットが版あたりの増分の半分以上を占めるときだけ実装する。満たさなければ測定を記録して閉じる。

**データの形。**

- 新しいエントリ `tables/<hash>.json`（新規）：1つの `TileMap` の正規の JSON（今の `TileMap` の serde と同じ）。`hash` は `ContentHash::of`（BLAKE3）。スナップショットと同じく JSON で、`objects/` ではない。
- スナップショットの中のタイル表は、`TILE_TABLE_INLINE_MAX = 1024`（新規。暫定、測定で決める）件を超えると `{ "table": "<hash>", "count": <件数>, "format": "rgba8" | "r8" }` に置き換える。以下は今の形のまま。
- メモリの中の `Document` は今と同じく全部の `TileMap` を持つ。置き換えは書くときと読むときだけ。

```rust
// crates/alto-doc/src/document.rs
pub struct SnapshotParts { pub snapshot: Vec<u8>, pub tables: BTreeMap<ContentHash, Vec<u8>> } // （新規）
impl Document {
    pub fn snapshot_parts(&self) -> Result<SnapshotParts, DocError>;                 // （新規）
    pub fn from_snapshot_parts(bytes: &[u8], tables: &dyn Fn(&ContentHash) -> Option<Vec<u8>>)
        -> Result<Self, DocError>;                                                    // （新規）
}
```

- 読むとき：表のバイトの BLAKE3 が名前と一致し、件数と `format` が合うこと。合わなければ `CORRUPT_DATA`。
- 全ての `TileMap` を同じ規則で扱う（ラスター、マスク、選択、効果の選択範囲。FX-8 が入れば `DisplacementMap` も）。`TileMap` を持つ場所を1つの関数 `Document::tile_maps_mut`（新規）で列挙し、新しい型はここに足す。

**保存と GC。**

- `PackageParts.tables`（新規）。`.alto`、薄い `.alto`、フォルダ形式の全てに、全ての版のスナップショットが参照する表を入れる（スナップショットと同じ扱い。薄い `.alto` でも外に出さない）。`external-objects.json` は変わらない。
- 読み手：`decode_entries` が `tables/` を読み、名前と内容を検査する。`openFolder` は `list("tables/")` を読む。
- GC：根から到達する表を残す（`prepare_gc`、`gc_folder`、`compact_archive`）。表はオブジェクトではないので、`doc_hashes` には入れず、新しい `table_hashes`（新規）で集める。
- 復旧のチェックポイントとジャーナルの記録：チェックポイントはパッケージと同じく表を持つ。ジャーナルの Transaction 記録は Op を持ち、`TileMap` を丸ごと持たない（`TileChange` 単位）ので変えない。版の記録（`version_record`）はスナップショットを持つので表も持つ。
- `alto-git` の写し（`crates/alto-git/src/mirror.rs`）：`tables/` をフォルダの他のエントリと同じく写す。

**差分。** `alto_doc::diff` はメモリの `Document` を比べるので変わらない。表を読まずに「変わったか」だけを知る最適化（表のハッシュの比較）は後続にする。

**上限。** 表1つの件数は今の文書の上限（辺 65536 → 1レイヤー最大 65,536 タイル）の内。表の JSON は最大約 6 MiB。`MAX_TILE_TABLE_BYTES = 16 MiB`（新規）を超える表は `CORRUPT_DATA`。

### 形式と通信

- `format_version`：上げる（11。00-ROADMAP の番号の表）。スナップショットの意味（表の参照）が増える。manifest の `document_format` も同じ値にする。
- manifest の `format`（2／3）：上げない案を既定にする。新しいエントリ `tables/` は今の読み手が無視するが、`document_format` の検査で先に拒まれるので、黙って壊れた文書として開かれることは無い。`format_version` は回 11（00-ROADMAP の番号の表）。manifest の `format` を上げないことは統合で確かめた。
- `PROTOCOL`：上げる（10。段階2の回）。`openFolder` の読み込みで `tables/` を渡す、版の操作で表を渡す。
- 描画の版：触れない。
- 共通規則3と 13 の QA-1：`format_version` を上げる PR で、`fuzz/seeds/snapshot/`、`tests/fixtures/*.alto`、`verification/*.alto` の seed を作り直す。

### 作業手順

Files：Modify `crates/alto-doc/src/document.rs`（`snapshot_parts`、`from_snapshot_parts`、`tile_maps_mut`）、`crates/alto-doc/src/lib.rs`（`format_version`）、`crates/alto-store/src/lib.rs`（`PackageParts.tables`、`package_sources`、`decode_entries`、`folder_entries`）、`crates/alto-engine/src/versions.rs`（`table_hashes`、保存と読み込み、`open_decoded_package` が表を渡す）、`crates/alto-engine/src/lib.rs`（`install_package` と `open_snapshot` がスナップショットだけでなく表も受け、`from_snapshot_parts` で読む）、`crates/alto-engine/src/versions/gc.rs`、`crates/alto-engine/src/versions/lazy.rs`（`target_docs` などスナップショットを読む箇所）、`crates/alto-git/src/lib.rs`（`from_snapshot_bytes` の3か所）、`crates/alto-git/src/mirror.rs`、`packages/alto-sdk/src/engine.ts`（`openFolder` の `tables/`）、`packages/alto-sdk/src/document.ts`（GC の `readAll` に `tables/`）。

Interfaces：Produces `SnapshotParts`、`Document::snapshot_parts`、`Document::from_snapshot_parts`、`PackageParts.tables`、`table_hashes`、`TILE_TABLE_INLINE_MAX`。

- [ ] 測定：判定条件の内訳を `doc/verification/` の結果文書に記録する。満たさなければここで止める。
- [ ] Rust 試験：`tile_table_over_the_limit_uses_a_table_entry`、`a_table_with_a_wrong_count_is_corrupt`、`gc_keeps_the_table_entry`、`a_thin_alto_keeps_every_table`（置き場なしで開いて log・diff・status が動く）、`diff_reads_tables_without_loading_pixels`。
- [ ] ブラウザ試験（`tests/e2e/image-version-tests.js`）：`a folder with tables opens on demand`、`a large layer survives save, open and version restore`。
- [ ] seed の作り直し（共通規則3、13 の QA-1）。
- [ ] 判定の再測定：同じ長時間試験で、版あたりの増分が下がったことを記録する。
- [ ] 検証：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし（測定が先）。
- 後に続く項目：STO-1（同じ `lazy.rs`、`gc.rs` を変える）、VER-5（08。履歴の打ち切りが GC の根を変える）、FX-8（02。`DisplacementMap` の `TileMap` を `tile_maps_mut` に足す。どちらが後でも、後の PR が足す）。（新規）

## STO-10 破損タイルの部分表示

### 現状

- 壊れたオブジェクト（復号できない、内容ハッシュが合わない）は `decode_entries` が警告に集めて `CORRUPT_DATA` にし、文書を開かない。参照されたオブジェクトの欠けは `install_package` が警告 `MISSING_TILE` を作るが、続く検査（`validate_node_content_impl`）が拒むので、結果は開かない。
- 警告の型（`WarningCode`、`packages/alto-sdk/src/types.ts`）には `MISSING_TILE` と `PACKAGE_DAMAGED` が既にある。`PACKAGE_DAMAGED` を Rust で作る経路は無い。
- 必要な分だけ読む文書（`openFolder` の `on-demand`、薄い `.alto`）では、版のオブジェクトの破損はその版を使うときに `CORRUPT_DATA` になる。
- 描画は、ストアに無いタイルを透明として合成する（共通の現状）。破損の目印を描く経路は無い。
- 文書を読み取り専用にする仕組みは無い（`savedFolder.readOnly` は置き場への書き込みだけを止める）。
- manifest が壊れていれば `CORRUPT_DATA`（`manifest missing`、`manifest: ...`）。有効なスナップショットを探し直す手段は無い。

### 11-04 からの変化

- 11-04 版は「表示経路の専用シェーダーで格子を描く」としていた。描画は `upload` に1か所でタイルを引く形（725d6f4 の分割の後の `upload.rs`）なので、シェーダーを足さず、目印のタイルをアトラスへ上げる形に変えた（モード番号が要らない）。
- F-9 で開く入口に `history` が加わった。`onCorrupt` は同じ選択肢の並びに置く。薄い `.alto` の版のオブジェクトは置き場にあるので、置き場の破損は今どおり版を使うときに分かる。（新規）

### 設計

**入口。**

- `engine.open(source, { history?, onCorrupt?: "refuse" | "placeholder" })`、`engine.openFolder(provider, { history?, onCorrupt? })`（`onCorrupt` は（新規））。既定は `refuse`（今の振る舞い）。
- `placeholder` で許すのは、作業文書と版のオブジェクトの「壊れている」と「無い」だけ。manifest、作業文書のスナップショット、到達する commit とスナップショット、`external-objects.json` の破損は今どおり拒む（構造が読めないため）。

**Rust。**

- `alto_store::decode_entries_tolerant(entries) -> Result<(Package, BTreeSet<ContentHash>), StoreError>`（新規）：壊れたオブジェクトを集めて返し、パッケージからは除く。
- `Engine::open_package_entries_damaged(entries) -> Result<(DocHandle, Vec<Warning>), EngineError>`（新規）：欠けと破損の集合を `damaged: BTreeSet<ContentHash>`（新規。文書ごと）に持ち、検査（`validate_node_content_impl` など）は damaged のハッシュを飛ばす。
- 警告：壊れたものは `PACKAGE_DAMAGED`（`details.hash`、`details.path`）、無いものは `MISSING_TILE`（`details.hash`）。件数は `MAX_DAMAGED_WARNINGS = 256`（新規）までで、超えた分は最後の1件の `details.more` に数を入れる。
- 読み取り専用：`Engine::is_read_only(h)`（新規）。damaged が空でない文書は、文書を変える全ての要求（編集、Undo／Redo、版の commit・checkout・merge、`save`、`saveFolder`）を `CONFLICT`（reason `document-damaged`（新規））で拒む。ジャーナルは書かない（編集が無い。チェックポイントは damaged のオブジェクトを持てない）。
- 許す操作：表示、`exportImage`（下）、log・diff・status、`save(target, { history: "omit", dropDamaged: true })`（新規）。後者は damaged のタイルを表から除いた作業文書だけの `.alto` を書き、`{ dropped: number }` を返す。書いた文書は普通の文書で、開き直せば編集できる。

**表示。**

- `TileStore` に `fn is_damaged(&self, hash: &ContentHash) -> bool { false }`（新規）。`Engine` は damaged の集合で答える。
- `Renderer::upload` と `upload_timed` は、damaged のハッシュに対して、ストアを読まずに目印のタイルをアトラスへ上げる。目印：RGBA8 は透明の上に 16 画素ごとの1画素幅の赤（255, 0, 0, 255）の格子、R8 は同じ位置が 255 で他は 0。CPU で一度作って使い回す。
- 目印は合成の入力として他のレイヤーと同じく合成される（表示だけ。正本には書かない）。新しいシェーダーとモード番号は要らない。
- `exportImage` は表示タイルを書き出すので、目印が写る。案：A（既定）写ったまま書き出す（EXPORT-CONTRACT の「表示の画素」と一致）。B 目印の代わりに透明を書き出す（表示と書き出しが食い違う）。既定 A を説明に書く。

**manifest の破損。**

- `engine.inspectDamaged(source: Blob | ArchiveSource | ObjectStorageProvider): Promise<RecoveryProposal[]>`（新規）：`snapshots/` と `commits/` を列挙し、読めて根が同じスナップショットを、commit の時刻（無ければ名前順）で新しい順に返す。`{ snapshot, commit?, objectsMissing: number }`。上限 `MAX_RECOVERY_PROPOSALS = 32`（新規）。
- 自動では開かない。ホストが提案を選び、`engine.openProposal(source, proposal, { onCorrupt })`（新規）で、そのスナップショットを manifest の代わりにして開く（読み取り専用。保存は `dropDamaged` の別名保存だけ）。

### 形式と通信

- `format_version`、manifest：触れない。
- `PROTOCOL`：上げる（10。段階2の回）。`open`／`openFolder` の `onCorrupt`、`inspectDamaged`、`openProposal`、`save` の `dropDamaged`。（新規）
- 描画の版：触れない（表示だけ。目印は正本と書き出しの契約の外）。

### 作業手順

Files：Modify `crates/alto-store/src/lib.rs`（`decode_entries_tolerant`）、`crates/alto-raster/src/lib.rs`（`TileStore::is_damaged`）、`crates/alto-engine/src/lib.rs`（damaged、読み取り専用の検査）、`crates/alto-engine/src/versions.rs`（開く経路）、`crates/alto-gpu/src/renderer/upload.rs`（目印）、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/engine.ts`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/errors.ts`（`document-damaged`）、`packages/alto-sdk/api/alto-engine.api.md`、`doc/design/EXPORT-CONTRACT.md`。

Interfaces：Produces `decode_entries_tolerant`、`TileStore::is_damaged`、`Engine::is_read_only`、`onCorrupt`、`inspectDamaged`、`openProposal`、`dropDamaged`。

- [ ] Rust 試験：`refuse_is_the_default`（今の破損の試験がそのまま通る）、`placeholder_opens_read_only_and_refuses_save`、`a_damaged_manifest_still_refuses`、`drop_damaged_writes_an_editable_copy`、`warnings_stop_at_256`。
- [ ] ブラウザ試験（`tests/e2e/image-version-tests.js`）：`a corrupt tile is drawn as a grid and reported`（壊したタイルの位置の表示タイルに赤の格子、警告 `PACKAGE_DAMAGED` 1件）、`edits on a damaged document are refused with document-damaged`、`inspectDamaged proposes the newest readable snapshot`。
- [ ] QA-1（13）：`fuzz/fuzz_targets/package.rs` に `decode_entries_tolerant` の呼び出しを足す（同じ入力で拒否と許容の両方を通す）。
- [ ] 完了条件：上の試験。既定の振る舞いが変わらない。
- [ ] 検証：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。STO-1 の後なら、部分読み込みで読み戻したオブジェクトの破損も damaged に入れられる（後から足す）。
- 後に続く項目：STO-1（同じ `upload.rs` と `TileStore` を変える。本項目を段階2で先に入れる。00-ROADMAP の重なりの表）。

## STO-11 PSD 往復

### 現状

- PSD を読む・書くコードは無い。画像の取り込みは `engine.assets.importImage`（`alto-decode`、Decode Worker）で、PNG などの1枚の画像だけ。
- 文書の要素：ラスター（straight RGBA8 のタイル）、マスク（R8）、グループ（`Isolation`）、不透明度、合成モード（`BlendMode`、17種。`crates/alto-doc/src/node.rs`）、効果（6種）、テキスト、ベクター。
- 外部の依存を足したら `THIRD-PARTY-NOTICES.md` を更新する決まり。Rust に Deflate の展開器は無い（COL-5／VEC-3 で `png` crate か `fdeflate` を足す設計が 01・07 にある）。
- 外部のバイト列の入口を足す PR は、同じ PR で fuzz の target と seed を足す（13 の QA-1 の運用の規則）。

### 11-04 からの変化

- 変化なし（PSD のコードは無い）。文書の要素は同じ。13 の QA-1 で fuzz の仕組み（`fuzz/`、`fuzz.yml`、`fuzz_regressions.rs`）が実装済みになり、`psd` の target をそこに足す形が決まった。

### 設計

**crate。** `crates/alto-psd/`（新規）。`alto-core`、`alto-doc` に依存し、GPU と WASM に依存しない。仕様の公開された範囲で自前で書く。

**読み込み。**

- 対象：PSD（version 1）。PSB（version 2）は `UNSUPPORTED`。色モード RGB、8bit。CMYK、Lab、16／32bit、インデックスは `UNSUPPORTED`（COL-6 の後に CMYK を見直す）。
- 要素の対応：

| PSD | Alto |
|---|---|
| 画素のレイヤー（チャネル R、G、B、透明 -1） | ラスター。straight RGBA8 へ |
| レイヤーマスク（チャネル -2） | マスク（R8）。既定色の外側を埋める |
| グループ（`lsct` の区切り） | グループ。通過（`pass`）は `PassThrough`、ほかは `Isolated` |
| 不透明度、表示 | `LayerProps.opacity`、`visible` |
| 合成モード `norm`、`mul `、`scrn`、`over`、`dark`、`lite`、`div `、`idiv`、`hLit`、`sLit`、`diff`、`smud`、`hue `、`sat `、`colr`、`lum `、`lddg` | `BlendMode` の同名の17種（新規）（`div ` は ColorDodge、`idiv` は ColorBurn、`smud` は Exclusion、`lddg` は Add） |

- 圧縮：Raw（0）と PackBits（1）。ZIP（2、3）は展開器が要るので、COL-5／VEC-3 の依存の決定まで `UNSUPPORTED` の警告にしてそのレイヤーを読まない。
- 非対応の要素（調整レイヤー、スマートオブジェクト、レイヤー効果、テキストの編集情報、ベクターマスク、上の外の合成モード）：警告の一覧で返す（`{ layer, kind, detail }`）。テキストは画素（ラスター）として読む。黙って捨てない。
- 上限：辺は文書の上限（65536）と PSD の 30000 の小さい方、文書は 64M 画素（画像の取り込みと同じ）、レイヤー `MAX_PSD_LAYERS = 4096`（新規）、ファイル 512 MiB（`.alto` と同じ）、展開後の作業量はレイヤーの画素の合計で 1 G 画素。超えたら `RESOURCE_LIMIT`。
- 結果は新しい文書（`Op` の列を1つの Transaction として作る）。

**書き出し。**

- ラスター、マスク、グループ、不透明度、表示、合成モードを書く。PackBits で圧縮。合成済みの画像（Image Data 節）は書き出しと同じく表示タイルから作る（64M 画素の上限）。
- テキスト、ベクター、効果は焼き込み（画素）で書き、編集できる形では残さない。焼き込みは IMG-1（属性付きの焼き込み）、IMG-2、IMG-3（05）の関数を使う。
- v3 の合成の見た目は Photoshop の結果と画素単位で一致しない前提にする。合成モードの式の違いを警告として返す。

**API。**

- `engine.openPsd(source: Blob | ArchiveSource): Promise<{ document: DocumentHandle, warnings: PsdWarning[] }>`（新規）。
- `doc.exportPsd(): Promise<{ blob: Blob, warnings: PsdWarning[] }>`（新規）。
- 解析は Engine Worker の WASM（`alto-wasm` に `alto-psd` を足す）で行う。

### 形式と通信

- `format_version`、manifest：触れない（PSD は文書の形式ではない）。
- `PROTOCOL`：上げる（13。段階4の回）。`openPsd`、`exportPsd`。（新規）
- 描画の版：触れない。

### 作業手順

Files：Create `crates/alto-psd/`（新規）、`fuzz/fuzz_targets/psd.rs`（新規）、`tests/fixtures/psd/`（新規。自作の PSD だけ）。Modify `Cargo.toml`（workspace）、`crates/alto-wasm/Cargo.toml`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/engine.ts`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`fuzz/Cargo.toml`、`.github/workflows/fuzz.yml`、`fuzz/run-all.sh`、`crates/alto-engine/tests/fuzz_regressions.rs`、`THIRD-PARTY-NOTICES.md`（依存を足す場合）。

Interfaces：Produces `alto_psd::read`、`alto_psd::write`、`PsdWarning`、`engine.openPsd`、`doc.exportPsd`。

- [ ] Rust 試験：`psd_header_and_layer_tree_parse`、`cmyk_psd_is_refused`、`zip_compressed_layer_is_listed_not_dropped_silently`、`unsupported_elements_are_listed`、`export_then_open_keeps_structure_and_pixels`（自作の文書を書いて読み、構造とラスターの画素が一致）、`psd_over_the_limits_is_refused`。
- [ ] fuzz：target `psd` を4か所にそろえる（13 の QA-1 の運用の規則）。
- [ ] ブラウザ試験（`tests/e2e/image-version-tests.js`）：`openPsd then exportPsd keeps layer names and blend modes`。
- [ ] Photoshop での実機確認は利用者の環境で行い、結果を `doc/verification/` に記録する（自動化しない）。
- [ ] 検証：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：IMG-1、IMG-2、IMG-3（05。書き出しの焼き込み）。ZIP 圧縮のレイヤーは COL-5／VEC-3（01、07）の展開器の決定。
- 後に続く項目：COL-6（01。CMYK の PSD を読む場合）。

## STO-12 外部保存先

### 現状

- 保存先の境界は `ObjectStorageProvider`（`packages/alto-sdk/src/storage.ts`）：`getObject`、`putObjects`（冪等）、`list`、`readRef`、`writeRef(name, data, expectHash?)`、任意の `exclusive`、`holdObjects`、`whileNotHeld`、`gcCapabilities`。実装は `MemoryObjectProvider`、`FileSystemDirectoryProvider`、`OpfsFolderProvider`。
- F-9 で、この境界は版の置き場（`save(target, { history: { folder } })`、`engine.open(source, { history })`）にも使われる。置き場の manifest がこの文書の知らない版を持つと、書き込みと GC は `CONFLICT`（reason `folder-changed`）になる。
- `gcCapabilities` を持たない保存先は GC が確認（dry-run）だけ。`holdObjects` が無い保存先は、他の文書の GC から守る手段が無い。
- ROADMAP の規則：外部へ送信する処理は SDK 本体に入れず、別パッケージに置く。`packages/` には `alto-sdk` だけがある。

### 11-04 からの変化

- `storage.ts` は変化なし。F-9（d05ed85、0443ac4 ほか、`document.ts`）で、保存先が版の置き場としても使われるようになった。Drive を版の置き場にする組み合わせを設計に加えた。

### 設計

- 別パッケージ `packages/alto-storage-gdrive`（新規）。npm 名は `@alto/storage-gdrive`（要確認）。`@alto/engine` を peer 依存にし、`ObjectStorageProvider` を実装するだけで、エンジンを変えない。
- 生成：`createDriveProvider({ folderId: string, getAccessToken: () => Promise<string>, fetch?: typeof fetch })`（新規）。トークンの取得と OAuth は呼び出し側が行う。
- 対応：`objects/…`、`snapshots/…`、`commits/…` はフォルダ内のファイル名（パスの `/` を `__` に置き換えた名前）。`manifest.json`、`document.json` などは ref。
- 冪等：`putObjects` は同じ名前のファイルがあれば、その中身を読んで、書こうとするバイト列と全バイトで比べ、一致すれば飛ばす。名前は内容アドレス（BLAKE3）だが、SDK に BLAKE3 は無い（`sha256Hex` だけ）ので、ハッシュは計算しない。一致しない同名のファイルは書き直さず `CORRUPT_DATA`。名前と中身の対応はエンジンが読むときに今どおり確かめる（`provide_objects` など）。
- 同名の重複：Drive は同じ名前のファイルを複数作れる。`list` と `getObject` は重複を見つけたら、名前に合う内容のものを1つ選び、どれも合わなければ `CORRUPT_DATA`。
- manifest の置き換え：`writeRef("manifest.json", data, expectHash)` は、期待ハッシュの比較と書き込みを不可分にできる場合だけ満たせる。Drive API でそれができるか（条件付きの更新）を実装の前に確かめる。できない場合は、書き込みの後に読み直して食い違いを `CONFLICT` にする弱い検出にとどめ、README にその旨を書く。どちらでも `exclusive` と `gcCapabilities` は持たない。
- GC：`gcCapabilities` を持たないので確認だけ。`holdObjects` も持たない（他の端末の GC を止められない）。版の置き場として使う場合も同じで、F-9 の「置き場の GC で古い薄い `.alto` の版が消えうる」は、Drive では GC を実行しないので起きない。
- エラー：401／403 は `STORAGE`（メッセージに `token`）、404 は `getObject` が `null`、429／5xx は指数で3回まで再試行してから `STORAGE`。
- 試験：実アカウントの試験は利用者の環境でのみ行う。CI は Drive API の模擬（`fetch` の差し替え）で試す。

### 形式と通信

- `format_version`、manifest、`PROTOCOL`：触れない。
- `@alto/engine` の公開 API：変えない。ネットワーク呼び出しが本体に増えないことを試験で確かめる。

### 作業手順

Files：Create `packages/alto-storage-gdrive/`（新規）（`package.json`、`src/index.ts`、`test/`、`README.md`）。Modify `.github/workflows/`（新しいパッケージの試験を足す）、`README.md`（外部送信の節に別パッケージを書く）。

Interfaces：Produces `createDriveProvider`。Consumes `ObjectStorageProvider`。

- [ ] node 試験（模擬 API）：`objects are idempotent`、`manifest write loses the race`（弱い検出の場合は「読み直しで気づく」）、`duplicate names are resolved by content`、`an expired token surfaces as STORAGE`、`a thin save uses Drive as the history store`（`MemoryObjectProvider` と同じ手順で薄い保存と開き直し）。
- [ ] `@alto/engine` にネットワーク呼び出しが増えていないことを試験（SDK のビルド出力（dist）の中に `fetch(` と `XMLHttpRequest` が無い）。
- [ ] 実アカウントでの確認は利用者の環境で行い、記録する。
- [ ] 検証：新しいパッケージの `test`、`pnpm --dir packages/alto-sdk build`。

### 依存

- 先に要る項目：なし（F-9 は実装済み）。
- 後に続く項目：VER-6（08。分散同期が保存先として使う場合）。

## Review Focus

- 読み込み中に Worker が再起動した場合に、ジャーナルの整合が保たれること（STO-1、STO-7）。
- 保存先が消えたり権限を失ったりした場合に、常駐から外したタイルが読めない状況のエラーの明示。編集中の作業は失わない（STO-1）。
- Undo が古いタイルを要求するときの読み込み待ちの間に、次の編集が来た場合の順序（STO-1。編集キューの中で直列）。
- `.alto` の範囲読みで、別プロセスがファイルを置き換えていないことの確認（STO-1、STO-5。`source-changed`）。
- ハッシュ検証を省略する経路を作らないこと（STO-1、STO-5、STO-9、STO-10）。
- F-9 との整合：薄い `.alto` の約束（置き場なしでも log・diff・status が動く、`external-objects.json` は版だけのオブジェクトと一致）を、STO-1（作業文書のオブジェクトを外す）、STO-5（新しいエントリ）、STO-9（タイル表を `objects/` に置かない）が壊さないこと。
- F-7 の pack の片付けが、STO-1 で外したオブジェクトの読み戻し元を消さないこと。
