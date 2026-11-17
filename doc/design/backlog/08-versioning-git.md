# 版管理と Git

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。現行仕様は [VERSIONING-AND-IMAGE-EDITING.md](../VERSIONING-AND-IMAGE-EDITING.md)、版を `.alto` の外に持つ保存（F-9）は [2026-11-16-versions-outside-alto.md](../2026-11-16-versions-outside-alto.md)、必要な分だけ読む履歴は [ON-DEMAND-HISTORY-OBJECTS.md](../ON-DEMAND-HISTORY-OBJECTS.md)。

**Goal:** 残るマージ機能、stash、履歴の打ち切り、同期、Git の双方向を、今の保存の形（完全な `.alto`、`.altoproj` フォルダ、薄い `.alto` と版の置き場）のまま足す。

**Architecture:** マージは3つの完全なスナップショットを JSON 値として比べる（`crates/alto-engine/src/versions/merge.rs` の `merge_value`）。版の状態を変える操作は Worker の `historyCommand` 1本に集まり、ジャーナルには版の状態遷移（`version_record_since`）として残る。過去の版のタイルは、メモリ、`.altoproj` フォルダ、薄い `.alto` の版の置き場のどこにあってもよく、使う直前に `ObjectTarget` で読む。`alto-git` は `alto-engine` にリンクしない CLI（`alto-engine` は試験だけの依存）。

## 共通の現状

- 版の型：`VersionStore`（`crates/alto-engine/src/versions.rs`）が `commits`、`commit_bytes`、`snapshots`、`head`、`refs: alto_store::VersionRefs`、GC の根になるオブジェクトの集合 `retained`、進行中のマージ `merge: Option<MergeSession>` を持つ。`VersionRefs`（`crates/alto-store/src/lib.rs`）は `current_branch`、`branches`、`tags`、`pinned`、`generation`、`gc_generation`。`VersionRefs::roots` がブランチの先頭、タグ、pin を返し、GC（`VersionStore::root_commits`）、開くときの必須コミット（`open_decoded_package`）、`log_dag`、`alto-git` の写像、置き場の検査（`store_roots_contained`）がすべてこれを使う。
- コミット：`Commit`（`crates/alto-doc/src/version.rs`）。`Commit::validate` は `format == 2`、親は2つまで、「親が2つ」と「`merge_base` が Some」が同値、メッセージは `MAX_MESSAGE` = 4096 文字まで。コミットのハッシュは正規化した JSON のバイト列のハッシュ。
- 参照名：`ref_name`（`crates/alto-engine/src/versions/branches.rs`）は NFC に正規化し、空、128バイト超、先頭の `-`、`/` `\` `~` `^` `:` `?` `*` `[`、空白、制御文字、`..`、`@{`、末尾の `.` と `.lock` を拒否する。名前空間を `/` で区切れない。
- マージ：`Engine::prepare_merge` が、現在の HEAD（`clean_head`：未コミットの変更があれば `CONFLICT`）と指定ブランチの共通祖先から「最良」の集合を求め、1つでなければ `MULTIPLE_MERGE_BASES`、無ければ `UNRELATED_HISTORY`。`merge_value` は値を再帰的に 3-way で比べる。`content`、`paths`、`effects`、`selection`、`transform`、`vector_mask`、`clip_path` は分けずに1つの値として比べる。ラスターで範囲（`bounds`）と画素（`tiles`）が両側で変わると `geometry` の競合。競合の種類は `conflict_kind` が決める（`pixels`、`asset`、`existence`、`parent`、`order`、`geometry`、`content`、`kind`、`property`）。統合した結果が `result_doc` を通らないときは、競合が無くても `prepare_merge` が文書全体の `structure` の競合を1つ足す。解決は `ours`、`theirs`、`explicit` の3種（`finish_merge`）。結果は `result_doc` で検証し、失敗は `INVALID_MERGE_RESULT`。
- プランは `MergeSession` に保持し、トークン、参照のダイジェスト、文書の版、作業文書のハッシュで古さを判定する（`STALE_MERGE`）。プランはジャーナルに残らない（再読み込みで消える）。確定（`finish_merge`）は `historyCommand` の `finish_merge` で行い、ジャーナルに残る。
- 必要な分だけ読む：`LazyObjects`（`crates/alto-engine/src/versions/lazy.rs`）が、フォルダに在ると分かっているオブジェクト `known`、操作中だけメモリに留める `pinned`、薄い `.alto` から開いたかの `external_history` を持つ。操作は `ObjectTarget`（`Commit`、`Branch`、`Merge`、`MergePlan`、`All`）を `require_objects` に渡し、足りなければ `STORAGE`（`objects-unavailable`）で何も変えずに断る。SDK の `withObjects` が `missingObjects` で足りないパスを聞き、フォルダから読んで `provideObjects` で渡す。`ObjectTarget::Merge` は両方の先頭と最良の共通祖先すべて（1段だけ）を読む。共通祖先の共通祖先は読まない（VER-1 で広げる）。
- 保存の形（F-9 の後）：
  - 完全な `.alto`：manifest `format` 2。`save(target)`、`save_package`。上限は `MAX_PACKAGE_BYTES` = 512 MiB。
  - `.altoproj` フォルダ：manifest `format` 2。`saveFolder(provider)` が、置き場の manifest のハッシュを期待値に `writeRef("manifest.json", …, expectHash)` で最後に書く。保存は編集キューの中で行う（PR #124）。文書が知らない版が置き場にあると、つなぐのは読むだけになり、書き込みと GC は `CONFLICT`（`folder-changed`）。
  - 薄い `.alto`：manifest `format` 3 と `external_history`（`ExternalHistory { document, objects, digest }`）、`external-objects.json`（版だけが参照するオブジェクトの辞書順の配列）。`save(target, { history: { folder } })` が版の置き場（`.altoproj`）を先に確定し、`mark_store_saved` で版のタイルをメモリから外してから `save_thin_package` を書く。開くのは `engine.open(source, { history })`、合わない置き場は警告 `HISTORY_STORE_UNAVAILABLE`。`decode_entries_impl` は `(format 2, external_history なし)` と `(format 3, あり)` だけを受け、他は `UNSUPPORTED`。フォルダに `external_history` があれば `decode_folder` が `UNSUPPORTED`。
- GC：`Engine::prepare_gc`（`crates/alto-engine/src/versions/gc.rs`）は manifest `format` 2 だけを受け、manifest の参照が文書と一致しなければ `CONFLICT`。根は `root_commits`、作業文書、Undo/Redo、`staged_tiles`。SDK の `gcFolder` は `gcCapabilities` が揃い `exclusive` がある保存先だけで実削除し、削除まで `exclusive` の中で行う。`whileNotHeld` で他の文書の `holdObjects` を待つ。
- ジャーナル：`historyCommand`（`crates/alto-wasm/src/image_versions.rs`、`enum HistoryCommand`）は `capture_version_state` → コマンド → `version_record_since` → 失敗なら `rollback_version_state`。記録は版の状態全体のチェックポイントで、遅延読み込みの文書では増分にしない。
- オブジェクトの列挙：`doc_hashes`（`crates/alto-engine/src/versions.rs`）→ `node_hashes`（`crates/alto-engine/src/lib.rs`）→ `effect_hashes`（`crates/alto-engine/src/selection.rs`）。ラスターのタイル、効果と調整の選択範囲、文字の焼き込み、マスク、選択、アセット。`alto-git` は別に、ラスター・マスク・アセットだけを数える `own_files` と、JSON の64桁の16進を拾う `referenced`（`crates/alto-git/src/lib.rs`）を持つ。
- 上限：Undo/Redo だけが持つオブジェクトは `MAX_HISTORY_ONLY_OBJECTS` = 2048（`crates/alto-engine/src/lib.rs`。版ではなく Undo の上限）。版の数、ブランチの数に上限は無い。`MILESTONES.md`「上限・継続課題」14、15 は「到達する古い履歴の打ち切りや分散同期は含めない」「ブラウザ内での git 操作、Git 側の変更の取り込み、rebase、cherry-pick は未実装」と書く。
- 打ち切りの席：`Engine::save_package_trimmed(h, ancestors)`（`crates/alto-engine/src/versions.rs`）は常に `UNSUPPORTED`。試験 `crates/alto-engine/src/versions_tests.rs` と `crates/alto-engine/src/lib.rs` がこれを確かめている。

## 共通の設計：参照の追加と manifest の形式

VER-4（stash）、VER-6（リモート）、GIT-1（Git から取り込んだ先頭）は、どれも `VersionRefs` に「ブランチではないが GC で残す先頭」を足す。3項目で同じ規則に従う。

- `VersionRefs` に足すフィールドは `#[serde(default, skip_serializing_if = …)]` で、空のとき manifest に出さない。既存のパッケージの manifest は1バイトも変わらない。
- 根を2種類に分ける。`VersionRefs::roots`（今のまま）は「Git に写す、共有する根」。新しい `VersionRefs::local_roots`（新規）は stash とリモートの先頭。GC、開くときの必須コミット、`validate_graph`、`store_roots_contained` は両方を使う（`VersionRefs::all_roots`（新規）= `roots` + `local_roots`）。`alto-git` の写像と `log_dag` の `all_branches` は `roots` だけを使う。
- `alto-git` の `mirror_manifest` は、Git に写すコミットの manifest から `local_roots` の参照を外す。外さないと、取り出した木が「参照されたコミットが無い」で開けない。（新規）
- manifest の `format`：`Manifest` は `deny_unknown_fields` で、`refs: VersionRefs` を `flatten` している。serde_derive はこの組み合わせでも残りのキーを検査するので、**今の読み手は refs に増えたキー（例：`stashes`）を持つ manifest を拒否する**（`unknown field`。`decode_entries_impl` では `CORRUPT_DATA`（`manifest: …`））。旧版が黙ってキーを捨て、GC で stash やリモートの版を消す危険は無い。それでも新しいフィールドを書く manifest の `format` は上げる。旧版が「壊れたパッケージ」ではなく `UNSUPPORTED`（`decode_entries_impl` の format の照合）を返し、新版が形式で読み方を分けられるようにするため。試験 `old_reader_refuses_unknown_ref_keys`（新規）で今の拒否を固定し、`new_ref_fields_raise_the_manifest_format`（新規）で format が上がることを固定する。
  - したがって新しいフィールドを持つ manifest は `format` を上げる（値は 4。下の案 B。案 A なら 4 と 5）。今の `format` は「2＝完全、3＝薄い」の2値で意味を持つので、組み合わせが増える。
  - 案 A：新しい番号を2つ（完全、薄い）割り当てる。
  - 案 B：新しい番号を1つ割り当て、その番号では `external_history` の有無で薄いかどうかを判定する。`decode_entries_impl` は `(2, なし)`、`(3, あり)`、`(新, どちらでも)` を受ける。`decode_folder`、`prepare_gc`、`adopt_gc` の `format != 2` の検査を「完全である」の検査に書き換える。
  - 既定：案 B。番号は 4（00-ROADMAP の番号の表）。今の試験 `crates/alto-store/src/lib.rs` の、format 4 を拒否すると確かめる行は、未知の番号（例：99）に変える。番号は「新しいフィールドのどれかが空でないか」だけで決める：stash、リモート、Git の先頭のどれか1つでもあれば新しい番号、すべて空なら旧番号（2 または 3）のまま書き、バイト列も今と同じ。どのフィールドがあるかで番号を分けない。
- `DocumentMeta::format_version`（文書の形式）は、この文書のどの項目でも上げない。文書の意味（ノード、効果）を変えないため。
- `PROTOCOL`：`historyCommand` の新しいコマンドと新しい要求を足す項目は上げる（VER-4 は 9、VER-5 は 10、GIT-2 は 11、VER-6 は 13。00-ROADMAP の番号の表）。Worker が知らないコマンドは `INVALID_ARGUMENT` になるので、SDK と Worker の版の食い違いを接続時に止める。

## VER-1 複数の共通祖先

### 現状

- `Engine::prepare_merge`（`crates/alto-engine/src/versions/merge.rs`）は、両方の祖先の共通部分から、他の共通祖先の親であるものを除いた「最良」の集合を求め、要素が1つでなければ `MULTIPLE_MERGE_BASES` で何も変えない。
- 試験 `crisscross_merge_bases_are_refused_and_dag_log_is_topological`（`crates/alto-engine/src/versions/branches.rs` の `dag_tests`）が、十字マージの後のマージが拒否され、版の状態のダイジェストが変わらないことを確かめる。
- 遅延読み込み：`ObjectTarget::Merge` の `target_docs`（`crates/alto-engine/src/versions/lazy.rs`）は両方の先頭と最良の共通祖先すべてを読む。共通祖先の共通祖先は読まない。試験 `merge_targets_cover_both_heads_and_the_base`。
- `MergePlan.base: Option<ContentHash>` と `MergeResult.base` は1つ。SDK の型（`packages/alto-sdk/src/image-types.ts`）も `base: string | null`。確定したマージのコミットは `merge_base: plan.view.base` を持ち、`Commit::validate` は親が2つなら `merge_base` を必須にする。`validate_graph` は `merge_base` が両方の親の祖先であることを検査する。

### 11-04 からの変化

`merge.rs`、`branches.rs` は変化なし。`lazy.rs` は F-9 で薄い `.alto` の関数が増えたが、`ObjectTarget::Merge` の対象は変わらない。11-04 版の試験の行番号（`branches.rs:690`）は古い。今は `dag_tests` の中の上記の試験。

### 設計

再帰マージの仮想ベース。

- 最良の共通祖先 B1…Bn（n ≥ 2）をハッシュの昇順に並べ、V1 = B1、Vk = 3-way(base = `virtual_base(Vk-1 と Bk の最良の共通祖先)`, ours = Vk-1, theirs = Bk) と順に畳む。共通祖先の共通祖先が複数なら同じ手順で再帰する。共通祖先が無いペアは、空の基準（`Document` の値を持たない `None`）で比べる代わりに、`MULTIPLE_MERGE_BASES` を返す（関係のない履歴を混ぜない）。
- 仮想マージの競合は確定させない。`merge_value` に方針の引数を足し、`ConflictPolicy::Collect`（新規。今の振る舞い）と `ConflictPolicy::TakeBase`（新規。競合した値は基準の値を採る。基準に無ければ値を除く）を分ける。仮想マージは `TakeBase` で行う。結果が `result_doc` を通らなければ `MULTIPLE_MERGE_BASES`（理由 `virtual-base-invalid`）。（新規）
- 上限（`merge.rs` に置く）：`MAX_MERGE_BASES`（新規）= 8（各段の最良の共通祖先の数）、`MAX_VIRTUAL_DEPTH`（新規）= 8（再帰の深さ）。超えたら今と同じ `MULTIPLE_MERGE_BASES`（理由 `too-many-bases`）。計算は祖先の集合を求める `VersionStore::ancestors` の繰り返しで、版の数に比例する。
- 仮想ベースはコミットにしない。値（`serde_json::Value`）として `MergeSession` に持つ。VER-2 の画素マージを仮想マージの中でも行うと、新しいタイルができる。そのタイルは `Engine::begin_tile_stage` の段に入れて GC から守る。段の持ち主は `Engine::staged_tiles` で、`MergeSession` は段の番号 `stage: u64`（新規）を持つだけ。今 `merge = None` で `MergeSession` を直接捨てる場所は6か所ある：`VersionStore::bump`（`branches.rs`）、`Engine::commit` と `invalidate_merge`（`versions.rs`）、`adopt_gc`（`gc.rs`）、`cancel_merge` と `finish_merge`（`merge.rs`）。`VersionStore` からは `Engine::release_staged_tiles` を呼べない。そこで捨てる処理を `Engine::drop_merge(h)`（新規。段を `release_staged_tiles` で外してから `merge = None`）に集め、6か所すべてがこれを通す。`bump` は `merge` を触らず、呼び出し側（`Engine` のメソッド）が `drop_merge` を呼ぶ。試験で、どの経路の後も `staged_tiles` に段が残らないことを確かめる。
- 確定したコミットの `merge_base` は B1（ハッシュが最小の最良の共通祖先）。どれも両方の親の祖先なので `validate_graph` を通る。`MergePlan` に `bases: Vec<ContentHash>`（新規）を足し、全部を返す。`base` は互換のため B1 を入れる。
- 遅延読み込み：`ObjectTarget::Merge` の `target_docs` を、仮想ベースの計算が読むすべての共通祖先（再帰の各段）へ広げる。上限で打ち切る段では読まない。`MergeSession::roots` も同じ集合を返し、GC の根にする。

Rust：

```rust
// merge.rs
const MAX_MERGE_BASES: usize = 8;    // 新規
const MAX_VIRTUAL_DEPTH: usize = 8;  // 新規
enum ConflictPolicy { Collect, TakeBase }  // 新規
fn virtual_base(&mut self, h: DocHandle, bases: &[ContentHash], depth: usize)
    -> Result<serde_json::Value, EngineError>;  // 新規
```

SDK：`MergePlan.bases: string[]`（新規）。

### 形式と通信

保存の形は変わらない（仮想ベースは保存しない）。`format_version`、manifest、描画の版は上げない。`MergePlan` に応答のフィールドを足すだけなので `PROTOCOL` は上げない。SDK の公開型が変わるので API 報告を更新する。

### 作業手順

**Files:** Modify `crates/alto-engine/src/versions/merge.rs`、`crates/alto-engine/src/versions/lazy.rs`（`target_docs`）、`crates/alto-engine/src/versions/branches.rs`（試験）、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`tests/e2e/image-version-tests.js`。

**Interfaces:** Produces `virtual_base`、`ConflictPolicy`、`MergePlan.bases`、`MergeSession.stage`、`Engine::drop_merge`。

- [ ] 既存の試験 `crisscross_merge_bases_are_refused_and_dag_log_is_topological` を、十字マージが仮想ベースで通る試験 `crisscross_merges_with_a_virtual_base` に変える。確定したコミットの `merge_base` が最良の共通祖先の小さい方であること。
- [ ] 試験 `virtual_base_conflict_takes_the_base_value`：仮想マージで競合した値が、利用者に競合として出ず、基準の値になる。
- [ ] 試験 `too_many_bases_is_refused`：`MAX_MERGE_BASES` を超えると `MULTIPLE_MERGE_BASES`、版の状態のダイジェストが変わらない。
- [ ] 試験 `virtual_base_tiles_survive_maintain_until_the_plan_ends`：仮想マージで画素を統合した後に `maintain` しても、プランのタイルが残る。`cancel_merge` の後は消える。
- [ ] 試験 `every_path_that_drops_a_plan_releases_its_stage`：`cancel_merge`、`finish_merge`、`commit`、ブランチの操作（`bump`）、Undo（`invalidate_merge`）、GC の確定（`adopt_gc`）の後に `staged_tiles` に段が残らない。
- [ ] 試験（`lazy.rs`）`merge_targets_cover_every_virtual_base`：遅延読み込みの文書で、共通祖先の共通祖先が `missing_objects` に入る。
- [ ] ブラウザ試験：十字マージを SDK から確定し、保存して開き直す。
- 完了条件：上の試験と共通規則7の検証がすべて通る。既存のマージの試験の結果が変わらない。
- 検証：`cargo test -p alto-engine versions`、`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に：なし。VER-2 と同時なら、仮想マージでも画素を統合する。VER-2 が後なら、VER-2 の PR で仮想マージにも画素の統合を入れる。
- 後に：VER-6（リモートとのマージで十字が起きやすい）、GIT-2（rebase、cherry-pick は同じ `merge_value` の方針を使う）。

## VER-2 画素内の自動マージ

### 現状

- タイルは `TileMap.tiles`（タイルの鍵 → ハッシュ）なので、別のタイルの編集は `merge_value` が既に自動で統合する。同じタイルを両側が変えると、その鍵で `pixels` の競合になる。試験 `different_tiles_merge_and_same_tile_conflicts`（`crates/alto-engine/src/versions/branches.rs`）。
- `pixels` の競合はラスターのタイルとマスク（`props.mask`）で起きる。選択（`selection`）と効果（`effects`）は1つの値として比べるので `property` の競合になり、画素の統合の対象にならない。02 の FX-8 の `DisplacementMap`（新規）は効果の中にタイルを持つが、同じく `effects` の値として比べられる。
- 範囲と画素の同時変更は `geometry` の競合（ノード全体）。
- 競合の値は `MergeValue { present, value }` で、タイルの競合ではハッシュの文字列。SDK の `MergeConflict.kind` は `string`。
- 確定（`finish_merge`）は結果のすべてのハッシュが `self.tiles.reachable` か `self.blobs` に在ることを確かめ、無ければ `MissingTile`。

### 11-04 からの変化

変化なし（`merge.rs` に 11-04 以降のコミットは無い）。遅延読み込みは `ObjectTarget::Merge` で3者のタイルを読むので、11-04 版の試験 `merge_with_lazy_objects_reads_tiles_from_the_folder` は既存の経路の確認になる。薄い `.alto` を置き場なしで開いた文書では `STORAGE`（`objects-unavailable`）になる（F-9）。（新規）

### 設計

`merge_value` の後に別の段 `merge_pixels`（新規）を置く。`merge_value` は同期で値だけを見るので、タイルの復号はここでまとめて行う。

- 対象：種類が `pixels` で、経路の最後がタイルの鍵の競合（ラスターの `tiles`、マスクの `tiles`）。`geometry` の競合、選択、効果は対象にしない。
- 3者のタイルを `alto_raster::load` で復号する。無いタイルはすべて0（透明）として扱う。形式は `TileMap.format`（RGBA8 または R8）。3者の形式が違えば統合しない（競合のまま）。
- 画素ごとの 3-way：`o == t` なら o、`b == o` なら t、`b == t` なら o、それ以外は競合の画素。比較はバイトの完全一致で、許容差を設けない。走査は行優先の固定順で、決定的。
- 全画素が統合できたら、結果のタイルを `stage_tile`（`MergeSession.stage` の段。VER-1 と共有）で入れ、結果の値のハッシュを置き換え、競合を外す。結果がすべて0なら鍵を消す（`stage_tile` は空のタイルに `None` を返す）。
- 一部の画素が競合したら、競合を残し、`MergeConflict.detail`（新規。`#[serde(skip_serializing_if = "Option::is_none")]`）に次を入れる。
  - `pixels: u32`（競合した画素の数）、`bounds: ImageBounds`（競合した画素の外接矩形。文書座標ではなくタイル内の座標）。
  - `ours_merged`、`theirs_merged`：競合した画素だけを ours / theirs にし、他は統合した2つのタイルのハッシュ。両方を段に入れる。（新規）
- 解決：`ours` と `theirs` は、`detail` があるとき `ours_merged` / `theirs_merged` を選ぶ（統合できた画素を失わない）。`explicit` は今と同じく任意の値（呼び出し側が `put_tile` 済みのタイルのハッシュ）。マスクで画素ごとに選ぶ解決は入れない（案。既定は入れない。入れるなら `choice: "mask"` と R8 タイルのハッシュを足す）。（新規）
- 上限：`MAX_PIXEL_MERGE_TILES`（新規）= 4096（1回のマージで画素を比べるタイルの数）。超えた分は統合せず、タイル単位の競合のまま残し、`MergePlan.warnings`（新規）に `pixel-merge-limit` を入れる。1タイルずつ復号して比べるので、作業メモリはタイル3枚と結果2枚（RGBA8 で約 1.25 MiB）で上限がある。
- 範囲：画素の統合でラスターのタイルの集合が変わっても、`bounds` は `merge_value` の結果のままにする。`result_doc` の検証で矛盾が出たら `structure` の競合になる（既存の経路）。
- ジャーナル：確定は `finish_merge` の版の記録で、結果のタイルはチェックポイントのオブジェクトに入る。ジャーナルの再生は記録したタイルをそのまま使うので、再計算しない。

### 形式と通信

保存の形は変わらない（結果は普通のタイル）。`format_version`、manifest、描画の版は上げない。`MergeConflict.detail` と `MergePlan.warnings` は応答のフィールドの追加なので `PROTOCOL` は上げない。SDK の公開型が変わるので API 報告を更新する。

### 作業手順

**Files:** Modify `crates/alto-engine/src/versions/merge.rs`、`crates/alto-engine/src/versions/branches.rs`（試験）、`crates/alto-engine/src/versions/lazy.rs`（試験）、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`tests/e2e/image-version-tests.js`。

**Interfaces:** Produces `merge_pixels`、`ConflictDetail`（新規）、`MergeConflict.detail`、`MergePlan.warnings`、`MAX_PIXEL_MERGE_TILES`。Consumes `MergeSession.stage` と `Engine::drop_merge`（VER-1。先に入る方が作る）。

- [ ] 試験 `edits_in_different_pixels_of_one_tile_merge`：同じタイルの別の画素の編集が競合なしで統合され、確定後のタイルが両方の編集を持つ。
- [ ] 試験 `same_pixel_is_a_conflict_with_its_bounds`：同じ画素は競合で、`detail.pixels` と `detail.bounds` が正しい。`ours` で確定すると、競合しなかった画素は統合された値になる。
- [ ] 試験 `pixel_merge_is_deterministic_for_both_directions`：ours と theirs を入れ替えても、競合しない画素の結果が同じ。
- [ ] 試験 `mask_tiles_merge_by_pixel`、`selection_and_effects_stay_whole_conflicts`。
- [ ] 試験 `pixel_merge_limit_leaves_tile_conflicts`：上限を超えたタイルはタイル単位の競合で、警告が付く。
- [ ] 試験（`lazy.rs`）`merge_with_lazy_objects_reads_tiles_from_the_folder`：フォルダから読んだ3者のタイルで統合でき、読まないと `STORAGE`。
- [ ] 試験 `merged_tiles_replay_from_the_journal`：確定後にジャーナルを再生すると同じタイル（`version_state_digest` が一致）。
- [ ] ブラウザ試験：同じタイルの別の画素を2つのブランチで描き、SDK からマージする。
- 完了条件：上の試験と共通規則7の検証が通る。
- 検証：`cargo test -p alto-engine versions`、`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に：なし（VER-1 と段を共有する）。
- 後に：GIT-2、VER-6（同じマージを使う）。FX-8（02）の `DisplacementMap` は効果の値として比べるので、この項目の対象外のまま。（新規）

## VER-3 文字範囲の自動マージ

### 現状

- テキストノードは `NodeKind::Text { content: Box<TextContent>, bake: Option<ContentHash> }`（`crates/alto-doc/src/node.rs`）。JSON では `{"kind":"text","content":…,"bake":…}`。
- `merge_value` は `content` を1つの値として比べるので、両側が文字を変えると `content` の競合になる。`bake`（焼き込みの ALTB のハッシュ）も両側で変わるので、同じノードで `property` の競合がもう1つ出る。
- `TextContent`（`crates/alto-text/src/model.rs`）は `text`、`style`、`spans`、`frame`、`writing_mode`、`align`、`tcy`（縦中横の範囲）、`auto_tcy`。範囲の追従は今 `replace_range` の中にある。
- 焼き込みは `Engine::bake_text_ops`（`crates/alto-engine/src/lib.rs`）→ `bake_ops`（`crates/alto-engine/src/text.rs`）。`transact` の中で Op に対して行う。`finish_merge` は焼き込まない（両側の `bake` のどちらかがそのまま入る）。
- 差分の説明は `ContentDetail::Text`（`crates/alto-doc/src/version.rs`）。

### 11-04 からの変化

`merge.rs`、`model.rs`、`version.rs` は変化なし。06 の書き直しで `remap_range`（新規）の形が決まった：`Edit { start, end, added }`（UTF-16）、`RangePolicy::Style`（span、圏点、割注）と `RangePolicy::Group`（縦中横、ルビ）。06 は `TextContent` にルビ、圏点、割注、段落属性などを足し、`ContentDetail::Text` にフラグを足す。

### 設計

`content` の競合のうち、3者がテキストノードの内容であるものを `merge_text`（新規）で統合する。

- 文字：base→ours と base→theirs の差分を UTF-16 の Myers diff で求め、各編集の端を書記素の境界へ広げる（`unicode-segmentation` は `alto-text` が既に使う）。編集を `Edit { start, end, added }` の列にする。
- 両側の編集の区間（base 上の `[start, end)`。挿入は長さ0の区間）が重ならなければ、両方を適用する。同じ位置への挿入は重なりとみなす（順序が決まらない）。重なれば `content` の競合のまま（今と同じ）。
- 範囲：base の各範囲（`spans`、`tcy`、06 が足すルビ、圏点、割注、段落属性）を、相手側の編集で `remap_range` する。片側だけが範囲を変えた場合、その側の範囲を他方の編集で追従させる。両側が同じ範囲を別の値に変えたら `content` の競合。範囲の種類ごとの規則は `RangePolicy` に従い、06 と同じ関数を使う（2つの実装を作らない）。（新規）
- 書式の値（`style`、`frame`、`writing_mode`、`align`、`auto_tcy`、06 が足すフィールド）：フィールドごとに `merge_value` で 3-way。値の競合は `content` の競合（経路は `content` の中のフィールド名まで）。
- 統合した `TextContent` は `TextContent::validate` を通す。上限（`MAX_TEXT_UTF16` 16384 など、`MILESTONES.md`「上限・継続課題」11）を超えたら `content` の競合のまま。
- `bake`：テキストの統合を行ったノードは、結果の値から `bake` を外し、`bake` の `property` 競合も外す。`finish_merge` で、`bake` の無いテキストノードを焼き込む。焼き込みは `bake_ops` と同じ関数で行う（06 で `bake_ops` と `bake_text` が `Result` になる。焼き込みの上限を超えたら `finish_merge` を `RESOURCE_LIMIT` で断り、何も変えない）。
- `ContentDetail::Text` は変えない（差分の表示は 06 の項目が持つ）。
- Undo：マージの確定は今と同じく版の切り替え（`install_version_document`）で、Undo 履歴を空にする。テキストの統合で Undo の単位は変わらない。

```rust
// merge.rs
fn merge_text(b: &TextContent, o: &TextContent, t: &TextContent)
    -> Result<TextContent, TextMergeConflict>;  // 新規
// alto-text の remap_range、Edit、RangePolicy（06 の TXT-5 で新規）を使う
```

### 形式と通信

保存の形は変わらない。`format_version`、manifest、描画の版、`PROTOCOL` は上げない。06 が `TextContent` にフィールドを足すたびに、`merge_text` の範囲の一覧に足す（06 の各項目の作業手順に含める）。（新規）

### 作業手順

**Files:** Modify `crates/alto-engine/src/versions/merge.rs`、`crates/alto-engine/src/text.rs`（焼き込みを関数で呼べるようにする）、`crates/alto-engine/src/versions/branches.rs`（試験）、`tests/e2e/image-version-tests.js`。

**Interfaces:** Consumes `remap_range`、`Edit`、`RangePolicy`（06 の TXT-5）。Produces `merge_text`。

- [ ] 試験 `edits_in_different_paragraphs_merge`：別の段落の編集が統合され、`bake` が結果の文字から作り直される。
- [ ] 試験 `overlap_is_a_conflict`、`same_position_inserts_are_a_conflict`。
- [ ] 試験 `merge_does_not_split_a_grapheme`：結合文字と絵文字の ZWJ 列で、編集の端が書記素の境界に広がる。
- [ ] 試験 `spans_follow_the_other_sides_edit`：片側の span が、他方の挿入で `remap_range` どおりに動く。
- [ ] 試験 `tcy_group_breaks_when_the_other_side_edits_inside`（`RangePolicy::Group`）。
- [ ] 試験 `text_merge_over_the_limit_stays_a_conflict`。
- [ ] 試験 `text_merge_has_no_separate_bake_conflict`：統合したノードに `bake` の競合が出ない。
- 完了条件：上の試験、`tests/parity/text-bake.txt` の既存行が変わらないこと、共通規則7の検証。
- 検証：`cargo test -p alto-engine versions`、`cargo test -p alto-text`、`sh scripts/check.sh native`。

### 依存

- 先に：TXT-5（06。`remap_range` と `Edit`、`RangePolicy`）。06 の `bake_ops` を `Result` にする変更（TXT の焼き込みの上限）。（新規）
- 後に：TXT-1、TXT-2、TXT-3、TXT-6、TXT-7 など、06 で `TextContent` に範囲を足す項目は、`merge_text` の範囲の一覧に足す。COLLAB-1（12。`merge_text` を呼び、`Result` を `.ok()` で包む）。（新規）

## VER-4 stash

### 現状

- 未コミットの作業を退避する手段は無い。ブランチの切り替え（`checkout`、`detach`）、マージは `clean_head` で、未コミットの変更があれば `CONFLICT`。
- ブランチの切り替えは `switch_version` → `install_version_document` で作業文書を版の内容に置き換え、Undo 履歴を空にする（`alto_doc::Editor::new`）。
- 版の状態を変える操作は `HistoryCommand`（`crates/alto-wasm/src/image_versions.rs`）の列挙に1つずつ足し、SDK の `historyCommand`（`packages/alto-sdk/src/document.ts` の `DocumentHandle`）で呼ぶ。
- コミットの作成は `Engine::commit`。同じスナップショットなら `"nothing changed since the last version"`（`INVALID_ARGUMENT`）。

### 11-04 からの変化

`branches.rs` は変化なし。ジャーナル（`journal.rs`）は F-4 で版の記録が増分になったが、遅延読み込みの文書では完全な記録のまま。保存は F-9 で薄い `.alto` が増え、manifest に `external_history` が加わった。stash の参照も manifest に入るので、「共通の設計」の manifest の形式の規則に従う。11-04 版の `refs.stashes` を `Manifest` の `VersionRefs` に足す点は同じ。

### 設計

未コミットの作業を、参照付きの非公開コミットに退避する。

- 型（`crates/alto-store/src/lib.rs`）：

  ```rust
  pub struct StashEntry { pub commit: ContentHash, pub message: String, pub created_at: i64 }  // 新規
  // VersionRefs に追加
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub stashes: Vec<StashEntry>,  // 新規。新しいものが先頭
  ```

  `local_roots`（新規）に `stashes` の `commit` を入れる。GC、開くとき、`validate_graph` が根として扱う。`alto-git` は写さない（「共通の設計」）。
- stash のコミット：`Commit { format: 2, parents: [head], snapshot: 作業文書, message, intents: 作業中の意図, created_at, author, merge_base: None }`。普通のコミットと同じ検証。ブランチは動かさない。
- `Engine::stash(h, message, created_at, author)`（新規）：HEAD が要る（無ければ `NO_COMMIT`）。detached でもよい（ブランチを動かさないため）。作業文書が HEAD と同じなら `INVALID_ARGUMENT`（`"nothing to stash"`）。HEAD の版のオブジェクトが要る（`require_objects(Commit { head })`。無ければ `STORAGE`）。コミットを作って `stashes` の先頭に入れ、作業文書を HEAD の内容に戻す（`install_version_document`。Undo 履歴は空になる）。進行中のマージは捨てる（`bump`）。
- `Engine::stash_apply(h, commit, drop)`（新規）：作業文書が HEAD と同じであること（`clean_head`）。base = stash の親、ours = HEAD、theirs = stash のスナップショットで、マージと同じ 3-way（`merge_value`、VER-2、VER-3 を含む）。
  - 競合が無ければ、結果を作業文書へ1つの可逆トランザクションで適用する（`restore_version` と同じく `alto_doc::restore_ops` → `transact`。意図は `version.stash_apply`（新規））。1回の Undo で戻せる。`drop` なら stash を外す。
  - 競合があれば、`MergePlan`（`kind: "stash"`（新規））を返し、`MergeSession` の行き先を `MergeTarget::Stash { commit, drop }`（新規）にする。`finish_merge` は行き先で分岐する。今の確定の経路（`current_branch` が無ければ `STALE_MERGE`（`detached`）、コミットを作ってブランチと HEAD を動かす、`install_version_document`）は `MergeTarget::Branch` だけが通る。`MergeTarget::Stash` はブランチも HEAD も動かさず、コミットも作らず、上と同じトランザクション（`restore_ops` → `transact`）で作業文書に適用する。したがって detached でも確定できる。結果の検証（`result_doc`、`format_version == 5`、オブジェクトの存在、`validate_node_content`）は共通に行う。確定まで stash を外さない。
  - stash の親が HEAD の祖先でなくてもよい（3-way の base は stash の親）。共通祖先を求めない。
- `Engine::stash_drop(h, commit)`（新規）：参照を外すだけ。オブジェクトは次の GC で消える。
- SDK（`doc.versions`）：

  ```ts
  stash(message?: string, options?: { author?: string }): Promise<StashEntry>;
  stashList(): StashEntry[];               // getSnapshot().versions.refs.stashes
  stashApply(commit: string, options?: { drop?: boolean }): Promise<MergePlan | null>;  // 競合なしは null
  stashPop(commit?: string): Promise<MergePlan | null>;   // = apply + drop。省略は先頭
  stashDrop(commit: string): Promise<void>;
  ```

  `stash` は `historyCommand` の `stash`（新規）で、`ObjectTarget::Commit { head }` を `withObjects` で読む。`stashApply` は `ObjectTarget::Stash { commit }`（新規。stash、その親、HEAD）を読む。競合の確定は既存の `finishMerge` をそのまま使う。
- 上限：`MAX_STASHES`（新規）= 64。超えたら `RESOURCE_LIMIT`。メッセージは `MAX_MESSAGE`。
- 保存：stash は manifest の参照なので、`.alto`、`.altoproj`、薄い `.alto`（版の置き場に stash のオブジェクトが入る。`version_only_objects` は `retained` から求めるので、stash のスナップショットのオブジェクトも外へ出る）のどれでも残る。置き場の検査（`store_roots_contained`）は `all_roots` を使う。（新規）
- ジャーナル：`historyCommand` の版の記録で残る。stash の作業文書のタイルはチェックポイントに入る。

### 形式と通信

- manifest：`stashes` を持つ manifest は `format` を上げる（4。「共通の設計」の案 B が既定）。stash が空なら今の形式のまま書く。
- `format_version`（文書）：上げない。描画の版：触れない。
- `PROTOCOL`：上げる（9。段階1の回）。`historyCommand` に `stash`、`stash_apply`、`stash_drop` を足す。（新規）
- SDK の型：`VersionRefs.stashes`（`packages/alto-sdk/src/image-types.ts`）、`StashEntry`、`MergePlan.kind` に `"stash"`。API 報告を更新する。（新規）

### 作業手順

**Files:** Modify `crates/alto-store/src/lib.rs`（`StashEntry`、`VersionRefs`、`local_roots`、`all_roots`、manifest の形式）、`crates/alto-engine/src/versions/branches.rs`（`stash`、`stash_apply`、`stash_drop`、`validate_graph`）、`crates/alto-engine/src/versions/merge.rs`（`MergeTarget`）、`crates/alto-engine/src/versions/gc.rs`（根）、`crates/alto-engine/src/versions/lazy.rs`（`ObjectTarget::Stash`、`store_roots_contained`）、`crates/alto-engine/src/versions.rs`（`open_decoded_package` の必須コミット）、`crates/alto-wasm/src/image_versions.rs`（`HistoryCommand`）、`crates/alto-git/src/mirror.rs`（`mirror_manifest` で外す）、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`README.md`、`RELEASING.md`、`tests/e2e/image-version-tests.js`。

**Interfaces:** Produces `StashEntry`、`VersionRefs.stashes`、`VersionRefs::local_roots`、`VersionRefs::all_roots`、`Engine::stash`、`Engine::stash_apply`、`Engine::stash_drop`、`MergeTarget`、`ObjectTarget::Stash`、`doc.versions.stash`、`stashList`、`stashApply`、`stashPop`、`stashDrop`。

- [ ] 試験 `old_reader_refuses_unknown_ref_keys`（`alto-store`。今の読み手が未知の refs キーを持つ manifest を拒否することを固定する）、`new_ref_fields_raise_the_manifest_format`（stash を持つ manifest の format が新しい番号で、今の照合では `UNSUPPORTED`）。
- [ ] 試験 `stash_restores_head_and_keeps_the_work`、`stash_pop_reapplies_the_work_as_one_undo`。
- [ ] 試験 `stash_with_nothing_to_save_is_refused`、`stash_in_detached_state_returns_to_the_detached_head`、`too_many_stashes_is_refused`。
- [ ] 試験 `gc_keeps_stashed_objects`、`prune_keeps_stashes`、`stash_survives_save_reopen_and_journal_replay`。
- [ ] 試験 `stash_survives_a_thin_save_and_reopen_with_the_store`：薄い `.alto` と置き場で保存し、置き場ありで開き直して `stashPop` できる。置き場なしでは `STORAGE`。
- [ ] 試験 `conflicting_pop_keeps_the_stash_until_finish`、`conflicting_pop_finishes_while_detached`（detached で競合を解決しても `STALE_MERGE` にならず、ブランチと HEAD が動かない）。
- [ ] 試験（`alto-git`）`stashes_are_not_mirrored_to_git`：stash のある文書を `sync` し、取り出した木が開ける。
- [ ] 試験 `a_manifest_without_stashes_keeps_its_bytes`：stash が無い文書の manifest が今と同じバイト列。
- 完了条件：上の試験、共通規則7の検証、`PROTOCOL` の更新と README、RELEASING の更新。
- 検証：`cargo test -p alto-store`、`cargo test -p alto-engine versions`、`cargo test -p alto-git`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に：なし。manifest の `format` 4 と `local_roots`（新規）を最初に入れる項目である（段階1の独立項目ではなく、VER-6、GIT-1 の基盤）。`crates/alto-engine/src/versions/gc.rs` の `format != 2` の検査は本項目が書き換え、同じ行の `document_format != 5` は回 6 の PR（06）が書き換える（00-ROADMAP の重なりの表）。
- 後に：VER-6、GIT-1（同じ `local_roots` と manifest の形式を使う）。COLLAB-1（12。`stash_apply` に `collab-active` の拒否を足す）。（新規）

## VER-5 履歴の打ち切り

### 現状

- 到達する古い版を消す手段は無い。`prune_versions`（`crates/alto-engine/src/versions.rs`）と GC は、どの参照からも到達しない版だけを消す。
- `Engine::save_package_trimmed(h, ancestors)` は席だけで、常に `UNSUPPORTED`（`"history truncation is not supported; use reference-based GC"`）。WASM と SDK からは呼べない。
- F-9 で、`.alto` の大きさの問題（版が持つタイルで 512 MiB に届く）は、版を置き場へ出す薄い `.alto` で解決した。置き場は GC しても、到達する版のタイルを持ち続ける（`thin-soak-240` では240分で置き場が 1,372 MiB を超えて増え続けた。`doc/verification/ENGINE-HARDENING-RESULTS.md` の F-9）。打ち切りは、置き場を含めた全体の大きさを減らす残りの手段になる。

### 11-04 からの変化

`versions.rs` は F-9 で変わった（薄い `.alto` の読み書き、`package_parts_except` の引数）。`save_package_trimmed` の席は 11-04 にもあり、変化なし。11-04 版の SDK 名 exportTrimmed は、今の SDK の書き出しの名前（`save`、`saveFolder`）に合わせて改める。F-9 の後は、出力先に版の置き場を選べる必要がある。

### 設計

開いている履歴を書き換えない。打ち切った新しいパッケージを別に書く。

- 残すコミットの集合 K：
  1. 根 R = `all_roots` と HEAD。根は常に残す。（新規）
  2. `keep.commits = n`（1以上）：各根から親を幅優先で辿った距離が n 未満のコミット。`keep.since = t`（ミリ秒）：`created_at >= t` のコミット。両方を指定したら、両方を満たすものだけ。根から K の中だけを辿って届くものに限る。
  3. マージの閉包：K のマージコミット c（親が2つ）について、`merge_base` B を K に入れ、両方の親から B までのすべての経路のコミット（`ancestors(親) - ancestors(B) + {B}`）を K に入れる。`Commit::validate`（親が2つなら `merge_base` が要る）と `validate_graph`（`merge_base` が両方の親の祖先）を保つため。閉包で K が大きくなることを報告に書く。
  4. 片親だけが K の外にあるマージコミットは、3. で両親を入れるので生じない。
- 書き直し：K の中で、親が K の外にあるコミットは親を外す（根のコミットになる）。親が変わったコミットとその子孫は、親のハッシュを新しいものに置き換えて作り直す（ハッシュが変わる）。親が変わらないコミットはハッシュも変わらない。スナップショットとオブジェクトは変えない。書き直しは親より子が後になる順（`log_dag` の逆順）で行い、元のハッシュ → 新しいハッシュの表 `mapping` を作る。
- 元のハッシュの記録：作り直したコミットのメッセージの末尾に `Alto-Trimmed-From: <元のハッシュ>` を足す。`MAX_MESSAGE` を超えるなら足さず、報告の `mapping` にだけ残す（警告 `trailer-omitted`）。
- 参照：K に無いコミットを指す参照は無い（根は K に入る）。出力の `head`、ブランチの先頭、タグ、pin、残す stash の `commit`、残すリモートの先頭は、すべて `mapping` で新しいハッシュに置き換える。置き換えないと `open_decoded_package`（`referenced commit is missing`）と `validate_graph` が出力を拒否する。stash とリモート（`local_roots`）は出力に残すかを選べる（`keepLocal`、既定は残す）。（新規）
- 同じ文書：出力の文書の root NodeId は元と同じ。親が変わらなかったコミット（境界より上で、祖先がすべて K に入るもの）は元と同じハッシュなので、元との共通の祖先になる。そうしたコミットが無ければ元とのマージは `UNRELATED_HISTORY`。在れば元とマージでき、書き直したコミットは元と別の版として並ぶ。
- 出力先：
  - `target: ArchiveTarget`：完全な `.alto`。上限は `MAX_PACKAGE_BYTES`。manifest の `format` は「共通の設計」に従う：`keepLocal` で stash やリモートを残すなら新しい番号、残すものが無ければ 2。（新規）
  - `{ folder: provider }`：空の `.altoproj`。中身があれば `CONFLICT`（`folder-occupied`）。元の文書の置き場と同じ provider は `CONFLICT`（`same-folder`（新規の理由））。
  - 薄い `.alto` と新しい置き場の組は第一段階では作らない（案。既定は作らない）。
- 読むオブジェクト：K のスナップショットが参照するもの。遅延読み込みの文書では `withObjects` の `ObjectTarget::Commits { hashes }`（新規）で読む。置き場が無ければ `STORAGE`（`objects-unavailable`）。
- 元の文書は変えない。保存済みの状態、`savedFolder`、置き場の hold も変えない。
- 報告：`TrimReport { kept: usize, removed: usize, closure_added: usize, mapping: BTreeMap<ContentHash, ContentHash>, warnings: Vec<String> }`（新規）。
- エラー：`keep` が空、`commits` が0、数値でない → `INVALID_ARGUMENT`。版が無い → `NO_COMMIT`。

Rust：

```rust
pub struct TrimKeep { pub commits: Option<u32>, pub since: Option<i64>, pub keep_local: bool }  // 新規
pub fn trimmed_parts(&self, h: DocHandle, keep: &TrimKeep)
    -> Result<(PackageParts, TrimReport), EngineError>;  // 新規。save_package_trimmed を置き換える
```

SDK：

```ts
doc.versions.saveTrimmed(  // 新規
  target: ArchiveTarget | { folder: ObjectStorageProvider },
  options: { keep: { commits?: number; since?: number }; keepLocal?: boolean },
): Promise<TrimReport>;
```

- `alto-git`：打ち切ったフォルダを、元のフォルダを写した同じリポジトリに `sync` すると、文書の root が同じなので同じ `refs/alto/<document>/state` を使い、表の検査（`verify_table`）またはブランチの前進の検査（`check_mapped` の `fast_forward_only`）で `CONFLICT` になる。打ち切った履歴は別のリポジトリへ写す。この振る舞いを試験で固定し、`alto-git` の利用者向けの説明に書く。

### 形式と通信

保存の形は完全な `.alto` かフォルダ。manifest の `format` は、出力に stash かリモートが残るとき VER-4、VER-6 の新しい番号 4、残らなければ 2。この項目で新しい番号を作らない。`format_version`、描画の版は上げない。`PROTOCOL` は上げる（10。段階2の回）：打ち切ったパッケージを作る要求 `trimmedPackage`（新規）を足す。

### 作業手順

**Files:** Modify `crates/alto-engine/src/versions.rs`（`trimmed_parts`、`save_package_trimmed` を消す）、`crates/alto-engine/src/versions_tests.rs`、`crates/alto-engine/src/lib.rs`（試験の `save_package_trimmed` の行）、`crates/alto-engine/src/versions/lazy.rs`（`ObjectTarget::Commits`）、`crates/alto-wasm/src/versions.rs`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`crates/alto-git/tests/git.rs`、`doc/MILESTONES.md`（上限14の文）、`README.md`、`RELEASING.md`、`tests/e2e/image-version-tests.js`。

**Interfaces:** Produces `TrimKeep`、`TrimReport`、`trimmed_parts`、`ObjectTarget::Commits`、`doc.versions.saveTrimmed`。

- [ ] 試験 `trimmed_export_reopens_with_new_hashes`：残したコミットの数、根のコミットに親が無いこと、スナップショットのハッシュが変わらないこと、`head`・ブランチ・タグ・pin・stash が `mapping` どおりの新しいハッシュを指すこと。
- [ ] 試験 `untouched_roots_keep_their_hashes`：K の中で親が変わらないコミットのハッシュが同じ。
- [ ] 試験 `merge_closure_keeps_the_base`：十字でないマージで、境界の外の `merge_base` と経路が K に入り、開き直した文書の `validate_graph` が通る。
- [ ] 試験 `original_document_is_unchanged`：元の文書の版の状態のダイジェスト、保存済みの状態が変わらない。
- [ ] 試験 `trimmed_folder_refuses_an_occupied_folder`、`trimmed_lazy_document_reads_kept_objects_only`。
- [ ] 既存の試験の `save_package_trimmed` の `UNSUPPORTED` の確認を、`trimmed_parts` の試験に置き換える。
- [ ] 試験（`alto-git`）`a_trimmed_folder_does_not_sync_into_the_original_mapping`：`CONFLICT` で何も書かない。
- 完了条件：上の試験、共通規則7の検証、`MILESTONES.md` の上限14の文を更新。
- 検証：`cargo test -p alto-engine versions`、`cargo test -p alto-git`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に：なし。VER-4、VER-6 の `local_roots` があれば `keepLocal` で扱う（無ければ `keepLocal` は効果なし）。（新規）
- 後に：GIT-3（打ち切ったフォルダの写像の扱いを同じにする）。

## VER-6 分散同期

### 現状

- 同期は無い。複数の文書の版を合わせるには、同じ `.altoproj` を `saveFolder` で共有するしかない。別の文書から保存された置き場は、読むだけでつながり、書き込みと GC は `CONFLICT`（`folder-changed`）（F-9、`store_roots_contained`、`DocumentHandle.setFolder` の `readOnly`）。
- `ObjectStorageProvider`（`packages/alto-sdk/src/storage.ts`）：`getObject`、`putObjects`（既存のパスを飛ばす）、`list`、`readRef`、`writeRef(name, data, expectHash)`、任意の `exclusive`、`holdObjects`、`whileNotHeld`、`gcCapabilities`。オブジェクトは内容アドレスなので、転送は Git と同じ形にできる。
- フォルダの読み込みは `engine.openFolder`（`packages/alto-sdk/src/engine.ts`）が manifest、`snapshots/`、`commits/` を読み、`folderPlan` で要るオブジェクトを読む。遅延読み込みでは `list("objects/")` を `known` にする。
- マージは `prepare_merge(h, name)` でブランチ名だけを受ける。ブランチ名に `/` は使えない（`ref_name`）。

### 11-04 からの変化

`storage.ts` は変化なし。`document.ts` と `engine.ts` は F-9 で大きく変わった：置き場の hold（`holdObjects`、`keepHold`）、読むだけの接続（`readOnly`）、`saveFolder` を編集キューの中で行う修正（PR #124）。11-04 版の「ブランチは `remote/<name>` の名前空間へ」は `ref_name` が `/` を拒むので成り立たない。別のフィールドに置く。11-04 版が Files に挙げた `storage-core.ts` は OPFS の作業ジャーナルの実装で、同期では変えない。

### 設計

相手（リモート）は `.altoproj` 形式の完全なフォルダを持つ `ObjectStorageProvider`。共有フォルダ、OPFS、STO-12 の保存先のどれでもよい。サーバーを持たない。ネットワークは provider の中にあり、SDK 本体は provider のメソッドだけを呼ぶ（ROADMAP「外部送信」）。

**参照の型**（`crates/alto-store/src/lib.rs`）：

```rust
pub struct RemoteRefs {  // 新規
    pub branches: BTreeMap<String, ContentHash>,  // 相手のブランチ名 → 先頭
    pub fetched_at: i64,
}
// VersionRefs に追加
#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
pub remotes: BTreeMap<String, RemoteRefs>,  // 新規。鍵はリモートの名前（ref_name の規則）
```

`remotes` の先頭は `local_roots` に入る（GC で残す、`alto-git` に写さない）。（新規）

**fetch**（`doc.versions.fetch(remote, { name })`）：

1. 編集キューの中で、`remote.exclusive` があればその中で読む。相手の `manifest.json` を読み、ハッシュを控える。無ければ `NOT_FOUND`。
2. Rust の `fetch_plan`（新規）が相手の manifest と相手の `list("commits/")` を受け、こちらに無いコミットのパスを返す。SDK はそれと対応するスナップショットを読む。
3. Rust の `fetch_check`（新規）が、読んだコミットとスナップショットを検証する：名前とハッシュの一致（`bytes_match_name`）、`Commit::parse`、文書の root がこちらと同じ（違えば `CONFLICT`（`other-document`））、こちらの版と合わせた `validate_graph`、相手の根がすべて届くこと。こちらに無いオブジェクトのパスを返す。
4. オブジェクトを読む先：
   - こちらがフォルダにつながり、書き込める（`savedFolder` があり `readOnly` でない）なら、相手から読んだオブジェクトをこちらのフォルダへ `putObjects` で書き、`LazyObjects.known` に足す。メモリには入れない。フォルダの manifest は次の保存で更新する。それまでの間はこの文書の hold が GC を止める。
   - つながっていなければ、メモリへ読む（`provideObjects` と同じ検証）。合計が `MAX_PACKAGE_BYTES` を超えるなら読まずに `RESOURCE_LIMIT`（理由 `fetch-needs-a-folder`（新規））。保存できない状態を作らない。
5. 相手の manifest をもう一度読み、1. と違えば `CONFLICT`（`remote-changed`（新規））で 6. をしない。
6. `historyCommand` の `fetch_apply`（新規）で、コミット、スナップショット、`remotes[name]` をまとめて入れる。版の記録としてジャーナルに残る。失敗したら文書と版の状態は変わらない（`rollback_version_state`）。

原子的なのは文書と版の状態だけ。4. でこちらのフォルダへ書いたオブジェクトは、5. や 6. で失敗しても消さない（provider への書き込みは巻き戻せない）。どれも内容アドレスの不変のファイルで、manifest から参照されないので、フォルダの次の GC で消える。`known` には 6. が成功したときだけ足す。

**push**（`doc.versions.push(remote, branch, { name })`）：fast-forward のみ。

1. 相手に `exclusive` と `holdObjects` が要る。無ければ `UNSUPPORTED`（理由 `remote-not-exclusive`（新規））。GC と同じ排他の中で、オブジェクトの書き込みから manifest の確定までを行う。`exclusive` の外で書くと、相手の GC がまだ参照されていないオブジェクトを消し、`putObjects` は在るパスを飛ばすので、消えたオブジェクトを指す manifest ができる。
2. 相手の manifest を読む。相手の root が違えば `CONFLICT`（`other-document`）。相手が自分の置き場（`savedFolder` と同じ provider）なら `CONFLICT`（`same-folder`）。
3. 相手のブランチの先頭が、こちらのブランチの先頭の祖先でなければ `CONFLICT`（理由 `non-fast-forward`（新規））。同じなら何もしない。相手の先頭がこちらに無いコミットなら、同じく `non-fast-forward`（先に fetch する）。相手にブランチが無ければ作る（`protected` は false）。
4. 相手の現在のブランチ（`current_branch`）へ push するとき：相手の作業文書が相手の HEAD のスナップショットと同じなら、作業文書ごと進める（Git の `updateInstead` と同じ）。違えば `CONFLICT`（理由 `remote-dirty`（新規））。
5. Rust の `push_entries`（新規）が、相手の manifest と相手の `list` を受け、相手に無いコミット、スナップショット、オブジェクトと、新しい manifest を返す。新しい manifest は相手の参照のうち対象のブランチだけを変え、`generation` を1つ上げる。こちらの stash、リモート、他のブランチは含めない。
6. 不変のファイルを `putObjects` で書き、manifest を `writeRef("manifest.json", …, expectHash = 2. で読んだハッシュ)` で最後に書いて読み戻す（`writeFolder` と同じ）。比較用の `document.json` は 4. で作業文書を進めたときだけ書く。
7. 成功したら `remotes[name].branches[branch]` を進める（`historyCommand` の `fetch_apply` と同じ記録）。（新規）

**統合**：`prepare_merge` に参照の種類を足す。`MergeSource::Branch { name }` と `MergeSource::Remote { remote, branch }`（どちらも新規）。SDK は `prepareMerge(name)` を残し、`prepareMerge({ remote, branch })` を足す。`ObjectTarget::Merge` も同じ形に広げる。第一段階は自動マージを含めない（fetch の後に利用者が `prepareMerge` を呼ぶ）。

**上限**：1回の fetch / push で `MAX_SYNC_COMMITS`（新規）= 65536 コミット。超えたら `RESOURCE_LIMIT`。リモートの数 `MAX_REMOTES`（新規）= 16、1つのリモートのブランチ数は相手の manifest のまま（相手の検証に従う）。

**F-9 との整合**：
- 相手は完全なフォルダ（`format` 2 または「共通の設計」の新しい番号で `external_history` なし）。薄い `.alto` は相手にできない（`decode_folder` が拒否）。
- こちらが薄い `.alto` から開いた文書でも、置き場がつながっていれば fetch はその置き場へ書く。push は版のタイルを置き場から読む（`withObjects`）。
- こちらの置き場が読むだけ（`readOnly`）なら、fetch はオブジェクトをメモリへ読む（4. の後者）。

SDK：

```ts
doc.versions.fetch(remote: ObjectStorageProvider, options: { name: string }): Promise<FetchReport>;     // 新規
doc.versions.push(remote: ObjectStorageProvider, branch: string, options: { name: string }): Promise<PushReport>;  // 新規
doc.versions.removeRemote(name: string): Promise<void>;  // 新規
interface FetchReport { commits: number; objects: number; branches: Record<string, string> }  // 新規
interface PushReport { commits: number; objects: number; head: string }  // 新規
```

### 形式と通信

- manifest：`remotes` を持つ manifest は `format` を上げる（4。VER-4 と同じ番号。「共通の設計」）。
- `format_version`（文書）、描画の版：触れない。
- `PROTOCOL`：上げる（13。段階4の回）。`fetchPlan`、`fetchCheck`、`pushEntries`（いずれも新規の要求）と、`historyCommand` の `fetch_apply`、`remove_remote`（新規）を足す。
- `ObjectStorageProvider` の型は変えない。

### 作業手順

**Files:** Modify `crates/alto-store/src/lib.rs`（`RemoteRefs`、`VersionRefs.remotes`）、`crates/alto-engine/src/versions/branches.rs`（`fetch_plan`、`fetch_check`、`fetch_apply`、`push_entries`、`MergeSource`）、`crates/alto-engine/src/versions/merge.rs`（`prepare_merge` の参照）、`crates/alto-engine/src/versions/lazy.rs`（`ObjectTarget::Merge` の形）、`crates/alto-wasm/src/image_versions.rs`、`crates/alto-wasm/src/versions.rs`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`README.md`、`RELEASING.md`、`doc/MILESTONES.md`（上限14）、`tests/e2e/image-version-tests.js`。

**Interfaces:** Produces `RemoteRefs`、`VersionRefs.remotes`、`MergeSource`、`fetch_plan`、`fetch_check`、`fetch_apply`、`push_entries`、`doc.versions.fetch`、`push`、`removeRemote`、`FetchReport`、`PushReport`、`MAX_SYNC_COMMITS`、`MAX_REMOTES`。Consumes `VersionRefs::local_roots`（VER-4。先に入る方が作る）。

- [ ] 試験（Rust）`fetch_reads_only_missing_objects`、`fetch_verifies_every_hash`、`fetch_from_another_document_is_a_conflict`、`remote_refs_are_gc_roots`。
- [ ] 試験（Rust）`non_fast_forward_push_is_a_conflict`、`push_to_a_dirty_current_branch_is_a_conflict`、`push_entries_carry_no_local_refs`。
- [ ] 試験（SDK、`MemoryObjectProvider`）`push_loses_the_race_on_the_manifest_lock`：push の間に相手の manifest が変わると `CONFLICT` で、相手の manifest は有効なまま。
- [ ] 試験（SDK）`interrupted_push_leaves_the_remote_manifest_valid`：オブジェクトの書き込みの途中で失敗させ、相手を `openFolder` で開ける。
- [ ] 試験（SDK）`push_without_exclusive_is_refused`。
- [ ] 試験（SDK）`fetch_into_an_attached_folder_does_not_load_objects`：fetch の後、`externalObjects` が増え、`prepareMerge({ remote, branch })` が置き場から読む。
- [ ] 試験（SDK）`fetch_without_a_folder_over_the_cap_is_refused`。
- [ ] 試験（SDK）`fetch_survives_journal_replay`：fetch の後にページを読み直して復旧し、`remotes` とコミットが残る。
- 完了条件：上の試験、共通規則7の検証、README と RELEASING の更新。
- 検証：`cargo test -p alto-engine versions`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に：VER-4 の `local_roots` と manifest の形式（無ければこの項目で作る）。VER-1（十字マージが起きやすい。無くても動く）。（新規）
- 後に：GIT-1（Git から取り込んだ先頭を `remotes` に置く）、GIT-3、COLLAB-1、STO-12（09。相手になる保存先）。

## GIT-1 Git の変更の取り込み

### 現状

- `alto-git sync`（`crates/alto-git/src/mirror.rs` の `sync_with`）は Alto → Git の一方向。Alto のコミット1つを Git のコミット1つにし、メッセージに `Alto-Commit` と `Alto-Snapshot` のトレーラを付ける（`commit_stream`）。対応表は `refs/alto/<document>/state` の blob（`SyncState`、`STATE_FORMAT` = 1、`deny_unknown_fields`）。
- 写したブランチの Git の ref が対応表の値と違えば `CONFLICT`（`check_mapped`、`"was changed outside alto-git"`）。表の各行は `verify_table` がトレーラ、親の順序、木の中のコミットファイルで確かめる。
- フォルダの読み込みは `Opened`（`crates/alto-git/src/lib.rs`）。`Opened::from_git` は Git のリビジョンから manifest、コミット、スナップショットだけを読む（オブジェクトは読まない）。
- 完全性の検査：`own_files` はラスターのタイル、マスク、アセットだけを数える。文字の焼き込み、効果の選択範囲、選択のタイルは、木に在れば `referenced` で拾うが、無くても検査しない。
- 薄い `.alto` は `export` で拒否する（`"thin .alto です"`、F-9）。`sync` は `folder::read` で読むので、フォルダの manifest に `external_history` があれば `decode_entries` を通るが、`own_files` で欠けが分かる。
- `MILESTONES.md`「上限・継続課題」15：Git 側の変更の取り込みは未実装。

### 11-04 からの変化

`crates/alto-git` は F-9 で変わった：薄い `.alto` を書き出さない（`ea93ec4`）、`parts` が `external` を運ぶ（`8c6d2fd`）。`mirror.rs` の写像の規則は変化なし。11-04 版の「トレーラの無い Git コミットを新しい Alto のコミットとして受け入れる」は、どの Alto のブランチを動かすかが無かった。VER-6 の `remotes` に置く形に改める。

### 設計

`alto-git pull <folder.altoproj> [--branch <name>]`（新規）：写した Git のブランチに、`alto-git` 以外が積んだ Git コミットがあれば、その木のフォルダから Alto の版を取り込む。

- 対象：対応表の各ブランチ（`SyncState.branches`、トランク）について、今の Git の ref が表の値 `e.git` の子孫なら、`e.git..ref` の Git コミットを古い順に見る。子孫でなければ（巻き戻し、強制更新）`CONFLICT` で何も書かない。
- トレーラ `Alto-Commit` のある Git コミット：既にある版なら飛ばす。無い版なら、別の文書の写像の誤りとして `CONFLICT`。
- トレーラの無い Git コミット：木の中のフォルダ（`prefix`）を読み、完全なパッケージであることを確かめる。
  - `manifest.json`、`snapshots/`、`commits/`、`objects/` を `cat_batch` で読み、`alto_store::decode_entries` を通す（名前とハッシュの一致）。`external_history` があれば `CONFLICT`（薄いパッケージは取り込まない）。
  - 文書の root がローカルのフォルダと同じ。違えば `CONFLICT`。
  - 版のすべてのスナップショットについて、参照するすべてのオブジェクトが木に在る。列挙はエンジンと同じ規則でなければならない。`node_hashes` と `effect_hashes` を `alto-doc` に移し、`Document::object_hashes`（新規）にする。`alto-engine` の `doc_hashes` と `alto-git` の両方がこれを使う。02 の FX-8 の `DisplacementMap` のタイルもここで列挙する。
  - `document.json`（比較用の写し）は読まない。スナップショットと違えば警告（`document.json は正本ではありません`）を返す。
  - どれかが満たせなければ `CONFLICT` で、ローカルのフォルダも Git の ref も変えない。
- 書き込み：取り込む版のコミット、スナップショット、オブジェクトを、ローカルのフォルダへ `folder::LockedFolder` の排他の中で書く。manifest には、そのブランチの先頭を `remotes["git"].branches[<Alto のブランチ名>]` として足す（VER-6 の型）。ローカルの Alto のブランチは動かさない。統合は Alto で `prepareMerge({ remote: "git", branch })` を行う。
- 対応表：取り込んだ Git コミットを `SyncState.adopted: BTreeMap<String, ContentHash>`（新規。Git の oid → その木の manifest の head h）に記録する。そのブランチの行（`RefEntry`）は `git` を oid に、`alto` を h に進める（h は取り込んだ版としてローカルの Alto の DAG に入っている）。`verify_table` は `adopted` の行を、トレーラではなく「木の `commits/<h>.json` のハッシュが h と一致し、木の manifest の head が h」で確かめ、親の順序の検査はしない（Git の親は利用者が作ったもので、Alto の親と対応しない）。`STATE_FORMAT` を 2 に上げ、1 の状態を読める（`adopted` は空）。
- 次の `sync`：行の `alto` が h なので、Alto のブランチの先頭が h の子孫になるまで、`check_mapped` の `fast_forward_only`（`ancestors(先頭).contains(e.alto)`）が `CONFLICT` を返す（Git の「push の前に pull」と同じ）。行の `git` も oid に進めてあるので、Git の ref の照合（`e.git` との比較）は通る。`pull` が `git` だけを進めて `alto` を前のままにすると、この検査が働かず、`sync` が取り込んだ Git コミットを飛ばしてトランクを動かす。統合した後の `sync` は、h の Git の親として `adopted` の oid を使う。h の祖先で、`adopted` からしか届かない Alto のコミットは Git に作らない（どの ref からも届かず、`git gc` で消えて表が壊れるため）。
- 冪等：同じ状態で2回 `pull` しても何も書かない。
- 薄い `.alto`：`pull` も `sync` もフォルダだけを受ける。薄い `.alto` の置き場（`.altoproj`）は完全なフォルダなので対象になる。

### 形式と通信

- Alto の manifest：`remotes` を使うので、VER-6 と同じ形式の規則（manifest の `format` を上げる。値は 4）。
- `SyncState`：`STATE_FORMAT` を 1 から 2 へ（Git の中の `alto-git` 専用の blob。Alto の形式番号の割り当ての対象外）。
- `format_version`（文書）、描画の版、`PROTOCOL`：触れない（CLI だけ）。

### 作業手順

**Files:** Modify `crates/alto-git/src/mirror.rs`（`pull`、`SyncState.adopted`、`verify_table`、`commit_stream` の対象）、`crates/alto-git/src/main.rs`（`pull` の使い方）、`crates/alto-git/src/lib.rs`（`own_files` を `Document::object_hashes` に）、`crates/alto-doc/src/document.rs`（`Document::object_hashes`）、`crates/alto-engine/src/lib.rs`（`node_hashes` を委ねる）、`crates/alto-engine/src/selection.rs`（`effect_hashes` を委ねる）、`crates/alto-engine/src/versions.rs`（`doc_hashes`）、`crates/alto-git/tests/git.rs`、`doc/MILESTONES.md`（上限15）。

**Interfaces:** Produces `alto_git::pull`（新規）、`PullReport`（新規）、`Document::object_hashes`、`SyncState.adopted`。Consumes `RemoteRefs`（VER-6。先に入る方が作る）。

- [ ] 試験 `object_hashes_match_doc_hashes`：既存の試験の文書（ラスター、マスク、効果の選択範囲、文字の焼き込み、アセット）で、移した関数の結果が前と同じ。
- [ ] 試験 `pull_accepts_a_complete_foreign_commit`：別の複製で Alto が保存したフォルダを `git commit` し、`pull` で `remotes["git"]` に入り、Alto でマージできる。
- [ ] 試験 `pull_refuses_an_incomplete_tree`、`pull_refuses_another_document`、`pull_refuses_a_thin_package`。
- [ ] 試験 `pull_ignores_a_hand_edited_document_json`：警告が出て、取り込む内容はスナップショットのまま。
- [ ] 試験 `pull_is_idempotent`、`a_forced_git_ref_is_a_conflict_and_nothing_moves`。
- [ ] 試験 `pull_moves_both_git_and_alto_of_the_row`：`pull` の後の行の `git` が取り込んだ oid、`alto` がその木の head。
- [ ] 試験 `sync_after_pull_needs_the_merge`、`sync_after_merge_uses_the_adopted_commit_as_parent`：取り込んだ後のマージを `sync` し、Git の親が `adopted` の oid。`git fsck --strict` が通る。
- [ ] 試験 `state_format_1_still_reads`。
- 完了条件：上の試験と既存の `alto-git` の試験が通る。`MILESTONES.md` の上限15を更新。
- 検証：`cargo test -p alto-git`、`cargo test -p alto-engine`、`sh scripts/check.sh native`。

### 依存

- 先に：VER-6 の `RemoteRefs`（無ければこの項目で作る）、VER-4 の manifest の形式。（新規）
- 後に：GIT-3（同じ規則を TypeScript で持つ）。FX-8（02、段階4）は本項目の後に入り、`Document::object_hashes` に `DisplacementMap` を足す（00-ROADMAP の重なりの表）。（新規）

## GIT-2 rebase、cherry-pick

### 現状

- Alto のコミットはスナップショット全体なので、差分の適用ではなく 3-way マージで作れる。`merge_value`、`result_doc`、`finish_merge` の検査がそのまま使える。
- ブランチの保護（`Branch.protected`）は今、削除の拒否だけに使う（`delete_branch`）。最初の `commit` が作る `main` は保護される。
- マージのプランは1つの行き先（`MergeSession.target_branch`）だけを持つ。プランはジャーナルに残らない。
- `MILESTONES.md`「上限・継続課題」15：rebase、cherry-pick は未実装。

### 11-04 からの変化

`branches.rs`、`merge.rs` は変化なし。11-04 版の「rebase の途中の状態を文書の履歴状態遷移としてジャーナルに記録する」は、マージのプランがジャーナルに残らない今の設計と合わない。途中のコミットを枝に付けず、最後に1回で枝を動かす形に改める（再読み込みで rebase は消え、枝は元のまま）。

### 設計

**cherry-pick**（`doc.versions.cherryPick(hash)`）：

- 作業文書が HEAD と同じ（`clean_head`）、現在のブランチがある（detached なら `CONFLICT`）。
- 対象が親を2つ持つ（マージコミット）なら `UNSUPPORTED`（第一段階）。親が無い（最初のコミット）なら base を空にせず `INVALID_ARGUMENT`。
- base = 対象の親、ours = HEAD、theirs = 対象で 3-way（VER-1〜3 の統合を含む）。競合は `MergePlan`（`kind: "cherry_pick"`（新規））で返し、既存の `finishMerge` で解決する。行き先は `MergeTarget::CherryPick { commit }`（新規）。
- 確定は単親のコミット（親は HEAD）。メッセージは対象のメッセージ＋`Alto-Cherry-Picked-From: <hash>`（`MAX_MESSAGE` を超えるならトレーラを足さない）。作成時刻は今、著者は対象の著者。保護されたブランチでも行える（履歴を書き換えない）。
- 対象が既に HEAD の祖先なら `INVALID_ARGUMENT`（`"already applied"`）。

**rebase**（`doc.versions.rebase(onto)`）：

- 現在のブランチ X、`onto` のブランチの先頭 O。X が保護されていれば `CONFLICT`（理由 `protected`（新規））。作業文書が HEAD と同じであること。
- 対象の列：X の先頭から O との共通祖先までの、第一親を辿ったコミット（古い順）。列にマージコミットがあれば `UNSUPPORTED`。共通祖先が複数なら VER-1 の仮想ベースではなく `MULTIPLE_MERGE_BASES`（rebase の列が決まらない）。
- 各コミットを、直前の結果を ours として cherry-pick の規則で作る。新しいコミットは `VersionStore` に入れるが、ブランチも HEAD も動かさない（どの参照からも届かない）。`staged`（新規）の段で、作ったコミットの新しいタイルを GC から守る。
- 競合で止まる：`RebaseState { onto, todo, done, original }`（新規）を `VersionStore` に持ち、`MergePlan`（`kind: "rebase"`）を返す。利用者は `finishMerge` で解決し、`doc.versions.rebaseContinue()` で続ける。`rebaseAbort()` で状態と作ったコミットを捨てる（参照から届かないので次の `prune` で消える）。
- 全部が終わったら、1回の `historyCommand`（`rebase_finish`（新規））で X の先頭と HEAD を最後のコミットに動かし、作業文書を置き換える（`install_version_document`。Undo 履歴は空になる）。ジャーナルにはこの1回だけが残る。
- `RebaseState` はジャーナルにも保存にも残さない。ページを読み直すと rebase は無かったことになり、X は元のまま。途中で別の `historyCommand`、`commit`、編集が入ると `RebaseState` を捨てる（`bump` と `invalidate_merge` で）。（新規）
- 上限：`MAX_REBASE_COMMITS`（新規）= 1024。超えたら `RESOURCE_LIMIT`。

Rust：

```rust
pub fn cherry_pick(&mut self, h: DocHandle, commit: ContentHash, created_at: i64)
    -> Result<CherryPickOutcome, EngineError>;  // 新規。完了か MergePlan
pub fn rebase_start(&mut self, h: DocHandle, onto: &str, created_at: i64)
    -> Result<RebaseOutcome, EngineError>;  // 新規
pub fn rebase_continue(&mut self, h: DocHandle, created_at: i64) -> Result<RebaseOutcome, EngineError>;  // 新規
pub fn rebase_abort(&mut self, h: DocHandle) -> Result<(), EngineError>;  // 新規
```

SDK：`doc.versions.cherryPick(hash)`、`rebase(onto)`、`rebaseContinue()`、`rebaseAbort()`（すべて新規）。競合の解決は `finishMerge`。（新規）

`alto-git`：cherry-pick と rebase の結果は普通のコミットなので、`sync` は今の規則で写す。rebase で X の先頭が前の先頭の子孫でなくなるので、`sync` は `check_mapped` の `fast_forward_only` で `CONFLICT` を返す。Git 側も書き換えるかは利用者の決定（案：`alto-git sync --force-branch <name>`（新規）を足す。既定：足さない。rebase したブランチは Git では別名で写す）。

### 形式と通信

保存の形は変わらない（新しいコミットは普通のコミット。`RebaseState` は保存しない）。`format_version`、manifest、描画の版は上げない。`PROTOCOL` は上げる（11。段階3の回）：`historyCommand` に `cherry_pick`、`rebase_finish` を、要求に `rebaseStart`、`rebaseContinue`、`rebaseAbort`（新規）を足す。

### 作業手順

**Files:** Modify `crates/alto-engine/src/versions/merge.rs`（`MergeTarget`、cherry-pick の確定）、`crates/alto-engine/src/versions/branches.rs`（rebase）、`crates/alto-engine/src/versions.rs`（`VersionStore` の `RebaseState`）、`crates/alto-engine/src/versions/gc.rs`（作りかけのコミットの根）、`crates/alto-engine/src/versions/lazy.rs`（`ObjectTarget::CherryPick`（新規）、`ObjectTarget::Rebase`（新規））、`crates/alto-wasm/src/image_versions.rs`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`crates/alto-git/tests/git.rs`、`doc/MILESTONES.md`（上限15）、`tests/e2e/image-version-tests.js`。

**Interfaces:** Produces `cherry_pick`、`rebase_start`、`rebase_continue`、`rebase_abort`、`RebaseState`、`MergeTarget::CherryPick`、`doc.versions.cherryPick`、`rebase`、`rebaseContinue`、`rebaseAbort`。Consumes `MergeTarget`（VER-4 で作る。先に入る方が作る）。

- [ ] 試験 `cherry_pick_makes_a_single_parent_commit`、`cherry_pick_of_a_merge_is_unsupported`、`cherry_pick_already_applied_is_refused`。
- [ ] 試験 `cherry_pick_conflict_resolves_through_finish_merge`。
- [ ] 試験 `rebase_moves_the_branch_once`：途中のコミットはどの参照からも届かず、最後に1回でブランチが動く。ジャーナルの記録が1つ。
- [ ] 試験 `rebase_stops_at_a_conflict_and_aborts_cleanly`：中断後の版の状態のダイジェストが rebase の前と同じ。
- [ ] 試験 `rebase_refuses_a_protected_branch`、`rebase_with_a_merge_commit_is_unsupported`。
- [ ] 試験 `rebase_state_is_dropped_by_an_edit_and_by_reload`。
- [ ] 試験 `rebase_tiles_survive_maintain_until_finish_or_abort`。
- [ ] 試験（`alto-git`）`a_rebased_branch_is_a_conflict_in_sync`。
- 完了条件：上の試験、共通規則7の検証、`MILESTONES.md` の上限15の更新。
- 検証：`cargo test -p alto-engine versions`、`cargo test -p alto-git`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に：VER-1、VER-2、VER-3（あれば同じ統合を使う。無くても動く）、VER-4 の `MergeTarget`（無ければこの項目で作る）。（新規）
- 後に：なし。

## GIT-3 ブラウザ内 Git

### 現状

- Git への写像はネイティブ CLI の `alto-git sync` だけ（`git` コマンドと `git fast-import` を使う。`run_fast_import`）。
- 写像の規則：Alto のコミット1つに Git のコミット1つ、同じ順序の親、メッセージに意図の行と `Alto-Commit`、`Alto-Snapshot` のトレーラ（`commit_stream`）、木は各版のフォルダ（`own_files`、`mirror_manifest`）、ブランチとタグの ref の名前（`--ref-prefix`、既定 `alto`）、pin と detached の HEAD は `refs/alto/<document>/keep/<hash>`、対応表は `refs/alto/<document>/state`。
- SDK にネットワークの処理は無い。README は「外部へ送信する処理はない」と書く（ROADMAP「外部送信」は SDK 本体についてこれを保つ）。
- 薄い `.alto` は写さない（F-9 の `alto-git export` の拒否と同じ理由：版のタイルが無い）。

### 11-04 からの変化

`crates/alto-git` は F-9 で、薄い `.alto` を拒否するようになった。写像の規則は変化なし。11-04 版のパッケージ名 `packages/alto-git-browser`（`@alto/git`）（新規）は今も存在しない。

### 設計

別パッケージ `packages/alto-git-browser`（新規。npm 名 `@alto/git`）。SDK 本体に入れない（依存、認証、ネットワークが入るため）。（新規）

- 依存：isomorphic-git（版は導入時に固定し、`THIRD-PARTY-NOTICES.md` に追記）。Git の保存先は isomorphic-git の `fs`（呼び出し側が渡す。LightningFS、OPFS のアダプタなど）。
- 入力：`.altoproj` 形式の `ObjectStorageProvider`（SDK の型を `@alto/engine` から型だけ読む）。薄い `.alto` は受けない（`external_history` があれば `UNSUPPORTED`）。
- `sync(provider, { fs, dir, branch?, refPrefix? }): Promise<SyncReport>`（新規）：`alto-git sync` と同じ規則を TypeScript で実装する。同じ入力から同じ Git のコミット ID を作る（作成時刻、著者、メッセージ、木、親が同じなら同じ ID）。`SyncState` の JSON の形も同じにし、CLI とブラウザが同じリポジトリを交互に扱えるようにする。
- `push(…, { onAuth, url })`、`fetch(…)`（新規）：isomorphic-git の HTTP。認証は呼び出し側が渡す `onAuth` だけで、トークンを保存しない。
- GIT-1 の `pull` は第二段階（CLI の `pull` と同じ規則を移す）。
- 打ち切った履歴（VER-5）：CLI と同じく、元の写像へ写すと `CONFLICT`。
- オブジェクトの完全性は GIT-1 の `Document::object_hashes` と同じ列挙が要る。TypeScript に同じ列挙を書くか、WASM（`alto-engine` を含む）に問い合わせるかは実装時に決める（案：WASM の小さな関数を SDK の `unstable` から出す。既定：TypeScript で書き、Rust と同じ固定の試験データで一致を試験する）。（新規）

### 形式と通信

- Alto の保存の形、`format_version`、manifest、描画の版、`PROTOCOL`：触れない（読むだけ）。
- `SyncState` の JSON：CLI と同じ（GIT-1 が `STATE_FORMAT` 2 にしていれば 2）。
- 公開物：`@alto/engine` の公開物に isomorphic-git が入らないこと（バンドルの検査）。

### 作業手順

**Files:** Create `packages/alto-git-browser/`（新規。`package.json`、`src/index.ts`、`src/mirror.ts`、`test/`）。Modify `THIRD-PARTY-NOTICES.md`、`README.md`（別パッケージのネットワークの扱い）、`crates/alto-git/tests/git.rs`（比較用の固定データを書き出す試験）。（新規）

**Interfaces:** Produces `@alto/git` の `sync`、`push`、`fetch`。Consumes `ObjectStorageProvider`（型のみ）。

- [ ] Node 試験 `browser mirror produces the same commit ids as alto-git sync`：同じフォルダ（作成時刻と著者を固定）から、CLI とブラウザ版の Git のコミット ID、ref、`SyncState` が一致する。
- [ ] Node 試験 `unknown refs are a conflict`、`a re-sync writes nothing`、`a thin package is unsupported`。
- [ ] Node 試験 `the cli continues a mapping the browser started`（交互に使える）。
- [ ] 試験 `@alto/engine bundle has no isomorphic-git`：SDK の配布物に isomorphic-git の文字列が無い。
- 完了条件：上の試験、`pnpm --dir packages/alto-sdk build` が通る、配布物の確認。
- 検証：`pnpm --dir packages/alto-git-browser test`（新規）、`pnpm --dir packages/alto-sdk build`、`cargo test -p alto-git`。

### 依存

- 先に：VER-6（同期の形）、GIT-1（`pull` を第二段階で移す）。
- 後に：なし。

## Review Focus

- 遅延読み込みと薄い `.alto`：自動マージ（VER-1〜3）、stash、打ち切り、fetch、cherry-pick が、要るタイルを `ObjectTarget` で先に読み、無ければ `STORAGE`（`objects-unavailable`）で何も変えないこと。置き場なしで開いた薄い `.alto` で確かめる。
- 新しい参照（stash、リモート）が GC、開くときの必須コミット、`store_roots_contained` の根に入り、`alto-git` の写像に入らないこと。新しい参照を書く manifest の `format` が上がり、旧版が `CORRUPT_DATA` ではなく `UNSUPPORTED` を返すこと。
- マージや rebase が作るタイルが、プランの間 `staged_tiles` で守られ、終われば外れること。
- マージ結果のタイルとテキストの焼き込みが、ジャーナル復旧で同じになること。
- 計算量の上限：仮想ベース（8×8）、画素マージ（4096タイル）、rebase（1024）、同期（65536コミット）。
- push が相手の `exclusive` の中で、オブジェクトを先に、manifest を最後に、期待するハッシュつきで書くこと。
- 打ち切った履歴とリベースしたブランチを `alto-git` に写したときに `CONFLICT` で止まり、Git に何も書かないこと。
