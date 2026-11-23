# 版を `.alto` の外に持つ：実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 過去の版のタイルをフォルダ形式の置き場に書き、`.alto` には作業文書のタイルと版の一覧だけを入れる保存と、それを開く経路を加える。

**Architecture:** 店（`alto-store`）に manifest format 3 と `external-objects.json` を加え、`PackageParts` に外に置くオブジェクトの集合を持たせる。エンジンは薄い `.alto` を開くとき、外のオブジェクトを今の「必要な分だけ読む」の known として扱う（`open_decoded_package(…, Some(known))`）。SDK は `saveFolder` の書き込みを先に済ませ、確定してから薄い `.alto` を書く。開くときは置き場を確かめ、合えば `setFolder`、合わなければ警告を出してつながない。

**Tech Stack:** Rust（alto-store、alto-engine、alto-wasm）、TypeScript SDK `@alto/engine`、pnpm、ブラウザ試験 `scripts/verify-browser.mjs`。

**Spec:** [2026-11-16-versions-outside-alto.md](2026-11-16-versions-outside-alto.md)

## Global Constraints

- 既定の `save(target)` と `save(target, { history: "omit" })` の振る舞いは変えない。
- 普通の `.alto` とフォルダの manifest は format 2 のまま。薄い `.alto` だけが format 3。format 3 は `external_history` を必ず持ち、format 2 は持たない。それ以外の組み合わせは `UNSUPPORTED`。
- document_format 5、render v3 は変えない。画素は変わらない。
- `MAX_PACKAGE_BYTES`（512 MiB）は変えない。
- 旧形式からの移行は実装しない（リリース済みの形式がない）。
- `external-objects.json` は、版だけが参照するオブジェクトのハッシュの辞書順の配列。作業文書が参照するオブジェクトと重ならず、版だけが参照するオブジェクトの集合と過不足なく一致する。
- `external_history.digest` は `external-objects.json` のバイト列の `ContentHash::of`（BLAKE3）。
- 新しい警告コードは `HISTORY_STORE_UNAVAILABLE`、details の `reason` は `missing` か `other-document`。
- WASM のハッシュ（`wasm/SHA256SUMS`）は更新しない（リリースのときだけ）。
- 1つの PR に1つの段。PR の前に opencode でレビューする。結果は `doc/verification/ENGINE-HARDENING-RESULTS.md` と同じ書き方で記録し、生ログは `verification/out/<名前>/` に置く。

## Review Focus

1. **薄い `.alto` を置き場なしで開いて、同じ置き場へ薄い保存をする**：置き場は同じ文書だが `savedFolder` が無いので、`saveFolder` の確認で `CONFLICT`（reason `folder-occupied`）になる。黙って上書きしない。`versions.attachFolder` の後なら成功する。試験は Task 5。
2. **置き場の `.alto` を開いた後に、置き場が別の薄い保存で進んでいる**：manifest が新しくても、同じ文書で外のオブジェクトが全て在れば、つないで開ける。試験は Task 5。
3. **薄い `.alto` の `target.write` が失敗する**：置き場はすでに確定しているので、置き場はつないだまま、文書は保存済みにしない（`saveState` は `error`）。もう一度保存すると成功する。試験は Task 4。
4. **作業文書だけで版がない文書を薄く保存する**：外のオブジェクトは0件。それでも format 3 で、`external-objects.json` は `[]`。開き直せる。試験は Task 1 と Task 2。
5. **薄い `.alto` を普通の `save(target)` で保存し直す**：置き場がつながっていれば format 2 の完全な `.alto` になり、つながっていなければ `STORAGE`（reason `objects-unavailable`）で何も書かない。試験は Task 5。

---

## PR 1：店の形式（Task 1）

### Task 1: manifest format 3 と `external-objects.json`

**Files:**
- Modify: `crates/alto-store/src/lib.rs`（`Manifest`、`PackageParts`、`Package`、`package_entries`、`package_sources`、`manifest_bytes_with_refs`、`decode_entries_impl` の形式の確認）
- Test: 同じファイルの `mod tests`

**Interfaces:**
- Produces:
  - `pub struct ExternalHistory { pub document: alto_core::NodeId, pub objects: u64, pub digest: ContentHash }`（serde、`deny_unknown_fields`）
  - `Manifest` に `#[serde(default, skip_serializing_if = "Option::is_none")] pub external_history: Option<ExternalHistory>`
  - `pub struct ExternalObjects { pub document: alto_core::NodeId, pub objects: BTreeSet<ContentHash> }`
  - `PackageParts` に `pub external: Option<ExternalObjects>`（`Default` は `None`）
  - `Package` に `pub external: Option<ExternalObjects>`
  - `pub const EXTERNAL_OBJECTS: &str = "external-objects.json";`
  - `pub fn external_objects_bytes(objects: &BTreeSet<ContentHash>) -> Vec<u8>`（`serde_json::to_vec` で辞書順の配列、末尾の改行なし）

- [ ] **Step 1: 失敗する試験を書く**

```rust
#[test]
fn thin_package_round_trips() {
    // parts: snapshot と、作業文書の object 1つ、external に別のハッシュ2つ
    let bytes = encode_parts(&parts).unwrap();
    let entries = read_entries(&bytes).unwrap();
    let m: serde_json::Value = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    assert_eq!(m["format"], 3);
    assert_eq!(m["external_history"]["objects"], 2);
    assert_eq!(entries[EXTERNAL_OBJECTS], external_objects_bytes(&ext));
    let p = decode_entries(entries).unwrap();
    assert_eq!(p.external.unwrap().objects, ext);
}
#[test]
fn thin_package_with_no_versions_is_format_3() { /* external が空集合でも format 3、`[]` */ }
#[test]
fn full_package_stays_format_2() { /* external: None なら format 2、external_history も EXTERNAL_OBJECTS もない */ }
#[test]
fn thin_package_rejects_bad_external_list() {
    // それぞれ decode_entries が Err(Corrupt)：
    // digest が合わない / 件数が合わない / 辞書順でない・重複 / EXTERNAL_OBJECTS がない /
    // .alto の中にある object が external にも載っている
}
#[test]
fn format_and_external_history_must_agree() {
    // format 3 で external_history なし → Unsupported
    // format 2 で external_history あり → Unsupported
    // format 4 → Unsupported
}
#[test]
fn thin_streaming_writer_matches_entries() {
    // encode_parts_fetching の出力が write_entries(package_entries(&parts)) と同じバイト列
}
```

- [ ] **Step 2: 試験を動かして失敗を確かめる**

Run: `cargo test -p alto-store thin_ format_and_external full_package_stays`
Expected: FAIL（`ExternalObjects` などが未定義）

- [ ] **Step 3: 実装する**

- `manifest_bytes_with_refs` に `external: Option<&ExternalObjects>` を加え、`Some` なら `format: 3` と `ExternalHistory { document, objects: len, digest: ContentHash::of(&external_objects_bytes(..)) }`。既存の呼び出しは `None` を渡す（`manifest_bytes` も）。
- `package_entries` と `package_sources` は、`parts.external` が `Some` なら `EXTERNAL_OBJECTS` のエントリを入れる。`folder_entries` は `parts.external` が `Some` なら `Err(StoreError::Io("a folder holds every object"))`。
- `decode_entries_impl` の最初の形式の確認を「format 2 かつ `external_history` なし、または format 3 かつ `external_history` あり」にする。format 3 なら `EXTERNAL_OBJECTS` を取り出して、件数・digest・辞書順で重複なし・`.alto` の中の objects と重ならないことを確かめ、`Package.external` に入れる。`entries.retain(|n, _| n.starts_with("objects/"))` より前に取り出す。
- `ZIP_ENTRY_OVERHEAD` の見積もり（`package_payload`）は `package_sources` を通るので、追加のエントリも数えられる。

- [ ] **Step 4: 試験を動かして通ることを確かめる**

Run: `cargo test -p alto-store`
Expected: PASS（既存の試験も全て）

- [ ] **Step 5: コミット**

```bash
git add crates/alto-store/src/lib.rs
git commit -m "Store: thin .alto format 3 with external-objects.json"
```

---

## PR 2：エンジン（Task 2〜3）

### Task 2: 薄い `.alto` を作る・開く

**Files:**
- Modify: `crates/alto-engine/src/versions/lazy.rs`（薄い保存、外のオブジェクトの検証）
- Modify: `crates/alto-engine/src/versions.rs:582-590`（`open_entries` が `Package.external` を known として渡す）
- Modify: `crates/alto-engine/src/lib.rs:770-786`（`thin_package_bytes`）
- Test: `crates/alto-engine/src/versions/lazy.rs` の `mod tests`（既存の `fixture()` と `paint()` を使う）

**Interfaces:**
- Consumes: Task 1 の `ExternalObjects`、`PackageParts.external`、`Package.external`
- Produces:
  - `pub fn version_only_objects(&self, h: DocHandle) -> Result<BTreeSet<ContentHash>, EngineError>`：`retained ∖ doc_hashes(document)`
  - `pub fn save_thin_package(&mut self, h: DocHandle) -> Result<Vec<u8>, EngineError>`
  - `pub fn thin_package_bytes(&self, h: DocHandle) -> Result<u64, EngineError>`
  - `open_entries` / `open_package` は format 3 を受け、外のオブジェクトを `LazyObjects.known` にする

- [ ] **Step 1: 失敗する試験を書く**

```rust
#[test]
fn thin_package_holds_only_the_working_objects() {
    // fixture() を普通に開き、全オブジェクトを使える状態で save_thin_package
    // .alto の objects/ は head_tile だけ、external は {old_tile}
    // thin_package_bytes == 実際の bytes.len() と同じ見積もり（package_bytes と同じ規則）
}
#[test]
fn thin_package_opens_without_the_store() {
    // open_package(thin) が成功。log、status、diff(Working, Commit(old)) が使える
    // missing_objects(Commit{old}) == [object_path(old_tile)]
    // restore_version(old) は STORAGE objects-unavailable で、文書は変わらない
    // external_objects(h) == {old_tile}
}
#[test]
fn thin_package_external_list_must_match_versions() {
    // external に版の参照しないハッシュを加えた .alto → CORRUPT_DATA
    // external から old_tile を抜いた .alto → CORRUPT_DATA（history object missing）
}
#[test]
fn thin_package_of_a_document_without_versions() { /* external が空。開ける */ }
#[test]
fn thin_package_restores_after_objects_are_provided() {
    // 置き場の entries から old_tile を provide_objects して restore_version(old) が成功し、
    // 普通の .alto から開いたときと同じ StateDigest
}
```

- [ ] **Step 2: 失敗を確かめる**

Run: `cargo test -p alto-engine thin_package`
Expected: FAIL（`save_thin_package` が未定義）

- [ ] **Step 3: 実装する**

- `save_thin_package`：`package_parts_except(h, &BTreeSet::new(), BTreeSet::new(), false)` で parts を作り、`parts.external = Some(ExternalObjects { document: doc.root, objects: version_only_objects(h)? })`、`encode_fetching(parts, &doc_hashes(doc))` で書く（作業文書のオブジェクトだけを取り出す）。その後 `self.backed = true; self.maintain();`（`save_package` と同じ）。
- `thin_package_bytes`：同じ parts と `fetch_plan(&mut parts, &doc_hashes(doc))` で `package_payload`。作業文書のオブジェクトは常にメモリにあるので `Option` にしない。
- `open_entries`：`decode_entries` の後、`p.external` が `Some(ext)` なら、`ext.document` が作業文書の root と違えば `CORRUPT_DATA`。`open_decoded_package(p, false, None, Some(ext.objects.clone()))` で開き、開いた後に `version_only_objects(handle)? == ext.objects` でなければ文書を外して `CORRUPT_DATA`（`external-objects.json does not match the versions`）。

- [ ] **Step 4: 通ることを確かめる**

Run: `cargo test -p alto-engine`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add crates/alto-engine/src
git commit -m "Engine: save and open a thin .alto"
```

### Task 3: 置き場の確認

**Files:**
- Modify: `crates/alto-engine/src/versions/lazy.rs`
- Modify: `crates/alto-engine/src/warning.rs`（コード定数）
- Test: `crates/alto-engine/src/versions/lazy.rs` の `mod tests`

**Interfaces:**
- Consumes: Task 2 の `external_objects`、`LazyObjects.known`
- Produces:
  - `pub enum StoreMismatch { Missing, OtherDocument }`、`StoreMismatch::reason(&self) -> &'static str`（`"missing"`、`"other-document"`）
  - `pub fn check_history_store(&self, h: DocHandle, manifest_and_snapshot: BTreeMap<String, Vec<u8>>, present: &BTreeSet<ContentHash>) -> Result<Option<StoreMismatch>, EngineError>`：置き場の `manifest.json` と、それが指す `snapshots/<hash>.json` の2つを受ける。snapshot の root が作業文書の root と違えば `OtherDocument`。`external_objects(h)` のどれかが `present` に無ければ `Missing`。manifest が読めない・snapshot が無い・壊れているときも `Missing`（`Err` にしない：開くのは成功させるため）。
  - `pub const HISTORY_STORE_UNAVAILABLE: &str = "HISTORY_STORE_UNAVAILABLE";`

- [ ] **Step 1: 失敗する試験を書く**

```rust
#[test]
fn history_store_check() {
    // fixture の置き場（entries）から manifest と snapshot、objects の一覧を作る
    // 全部ある → None
    // old_tile を present から抜く → Some(Missing)
    // 別の文書の置き場 → Some(OtherDocument)
    // manifest が無い・壊れている → Some(Missing)
}
```

- [ ] **Step 2〜4: 失敗を確かめ、実装し、`cargo test -p alto-engine history_store_check` が通ることを確かめる**

- [ ] **Step 5: コミット**

```bash
git add crates/alto-engine/src
git commit -m "Engine: check a history store before attaching it"
```

PR 2 の終わりに、F-5 のベンチ（`cargo run --release -p alto-engine --example save_bench`）に薄い保存の回を加え、保存の最大メモリが作業文書の大きさで決まることを、同じ条件で前後20回ずつ測る（生ログ `verification/out/thin-save-bench/`）。

---

## PR 3：WASM と SDK（Task 4〜6）

### Task 4: 薄い保存

**Files:**
- Modify: `crates/alto-wasm/src/versions.rs`（`saveThinPackage`、`thinPackageBytes`）
- Modify: `packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/worker.ts`（要求 `saveThin`、`packageBytes` に `external?: boolean`）
- Modify: `packages/alto-sdk/src/document.ts:497-530`（`save`）、`:597-646`（`saveFolderLocked` を分ける）、`:474-487`（`packageSize`）
- Test: `tests/e2e/image-version-tests.js`

**Interfaces:**
- Consumes: Task 2 の `save_thin_package`、`thin_package_bytes`
- Produces:
  - WASM：`saveThinPackage() -> Vec<u8>`、`thinPackageBytes() -> f64`
  - Worker 要求：`saveThin: Q<{}, Uint8Array>`、`packageBytes: Q<{ external?: boolean }, { bytes: number; limit: number } | null>`
  - SDK：`save(target: ArchiveTarget, options?: { history?: "all" | "omit" | { folder: ObjectStorageProvider } })`
  - SDK：`packageSize(options?: { history?: "all" | "external" }): Promise<PackageSize>`。`"external"` は何も読まずに薄い `.alto` の大きさを返す。
  - `private writeFolder(owner, storage, options): Promise<{ report: { written: number; skipped: number; warnings: string[] }; token: number; manifest: string }>`：今の `saveFolderLocked` の書き込みと確認の部分。`saved`、`markFolderSaved`、`saveState` の通知はしない。`saveFolderLocked` はこれを呼んで今と同じことをする。

- [ ] **Step 1: 失敗するブラウザ試験を書く**（`image-version-tests.js` の on-demand の試験の後）

```js
await test("thin .alto: versions in a folder store", async () => {
  // 6つの版を作る（on-demand の試験と同じ作り方）
  // const store = new MemoryObjectProvider();
  // await d.save(target, { history: { folder: store } });
  // 薄い .alto は普通の .alto より小さく、その大きさは d.packageSize({ history: "external" }).bytes と同じ
  // store は単独で openFolder でき、log が同じ
  // d.dirty === false、saveState は saving → saved を1回ずつ
});
await test("thin .alto: a failed .alto write leaves the store attached and the document dirty", async () => {
  // target.write が throw → save は reject、saveState は error、d.dirty === true
  // もう一度 save(goodTarget, { history: { folder: store } }) が成功
});
```

- [ ] **Step 2: 失敗を確かめる**

Run: `node scripts/verify-browser.mjs --suite=image-versions`
Expected: FAIL（`history.folder` が未対応）

- [ ] **Step 3: 実装する**

`save` の `history` がオブジェクトのとき：`endTool` の後、`saveState` を `saving` にし、編集キューの中で（置き場が `savedFolder.provider` でなければ `withObjects({ type: "all" })` の中で）、置き場の `exclusive` の中で `writeFolder` を呼ぶ。終わったら `setFolder`（置き場が変わったときは `holdObjects` を取り直す。今の `saveFolderLocked` と同じ）。その後 `saveThin` で薄い `.alto` を受け取り、`target.write` に書く。成功したら `saved { token }`、`markFolderSaved`、`saveState` を `saved`、`packageSize` イベントを `external` の大きさで送る。失敗したら `releaseSave(token)`、`saveState` を `error` にして投げ直す。置き場は確定済みなのでつないだままにする。

- [ ] **Step 4: 通ることを確かめる**

Run: `node scripts/verify-browser.mjs --suite=image-versions` と `pnpm -C packages/alto-sdk build && node --test packages/alto-sdk/test/`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add crates/alto-wasm packages/alto-sdk tests/e2e/image-version-tests.js
git commit -m "SDK: save a thin .alto with its versions in a folder"
```

### Task 5: 置き場をつないで開く

**Files:**
- Modify: `crates/alto-wasm/src/versions.rs`（`checkHistoryStore`）、`crates/alto-wasm/src/lib.rs`（`self.warnings` に警告を加える）
- Modify: `packages/alto-sdk/src/protocol.ts`、`worker.ts`（要求 `checkHistoryStore`）
- Modify: `packages/alto-sdk/src/engine.ts:136,697-703`（`open`）、`packages/alto-sdk/src/document.ts:380-392`（`attachFolder`）
- Modify: `packages/alto-sdk/src/types.ts:394`（`WarningCode` に `"HISTORY_STORE_UNAVAILABLE"`）
- Test: `tests/e2e/image-version-tests.js`

**Interfaces:**
- Consumes: Task 3 の `check_history_store`、`StoreMismatch::reason`
- Produces:
  - WASM：`checkHistoryStore(framed: &[u8], objects: &str, warn: bool) -> String`。戻りは JSON `{ "mismatch": null | "missing" | "other-document", "state": WireState }`。`warn` が真で不一致なら `Warning::new(HISTORY_STORE_UNAVAILABLE).with("reason", …)` を `self.warnings` に加えてから state を作る。
  - Worker 要求：`checkHistoryStore: Q<{ bytes: ArrayBuffer; objects: string[]; warn: boolean }, { mismatch: "missing" | "other-document" | null; state: WireState }>`
  - SDK：`Engine.open(source: Blob | ArchiveSource, options?: { history?: ObjectStorageProvider }): Promise<DocumentHandle>`
  - `DocumentHandle` の内部：`private async checkStore(provider, warn: boolean): Promise<"missing" | "other-document" | null>`。置き場の `exclusive` の中で `manifest.json`、それが指す `snapshots/<hash>.json`、`list("objects/")` を読み、`checkHistoryStore` を呼んで state を `apply` する。一致したら `setFolder(provider, sha256Hex(manifest), hold)`。

- [ ] **Step 1: 失敗するブラウザ試験を書く**

```js
await test("thin .alto: open with and without the store", async () => {
  // 薄い .alto と store を Task 4 と同じに作る
  // e1 = await engine.open(thin, { history: store }) → warnings に HISTORY_STORE_UNAVAILABLE がない
  //   versions.restore(古い版) が成功し、普通の .alto から開いて同じ版に戻したときと pickColor が同じ
  // e2 = await engine.open(thin) → log・status・diff が e1 と同じ
  //   versions.restore(古い版) は STORAGE、details.reason === "objects-unavailable"
  //   versions.attachFolder(store) の後は成功
  // e3 = await engine.open(thin, { history: 別の文書の store }) → 開けて、警告 reason "other-document"、
  //   attachFolder(別の store) は CONFLICT
  // e4 = await engine.open(thin, { history: 古い版の object を消した store }) → 警告 reason "missing"
});
await test("thin .alto: review focus", async () => {
  // (1) 置き場なしで開いた薄い .alto を同じ store へ薄く保存 → CONFLICT reason "folder-occupied"、
  //     attachFolder(store) の後は成功
  // (2) 古い薄い .alto を、store がもう1回薄く保存された後に store ありで開く → つながる
  // (5) 置き場ありで開いた薄い .alto の save(target) → format 2 の完全な .alto（open して restore が置き場なしで成功）
  //     置き場なしで開いた薄い .alto の save(target) → STORAGE objects-unavailable、target.write は呼ばれない
});
```

- [ ] **Step 2: 失敗を確かめる**（`node scripts/verify-browser.mjs --suite=image-versions`）

- [ ] **Step 3: 実装する**

`Engine.open`：今と同じに開いたあと、`options.history` があり、`versions.externalObjects() > 0`（format 3 で外がある）なら、`holdObjects` を取ってから `checkStore(provider, true)`。不一致ならホールドを放す。format 2 の `.alto` に `history` を渡されたときは何もしない（読むものがない）。`attachFolder`：今の `missingObjects` の確認の前に `checkStore(provider, false)` を呼び、`other-document` なら `CONFLICT`（reason `folder-occupied`、今と同じ理由）。

- [ ] **Step 4: 通ることを確かめる**（同じコマンドと `pnpm -C packages/alto-sdk run api:check`。API の報告が変わるので `pnpm -C packages/alto-sdk run api` で更新してコミットに含める）

- [ ] **Step 5: コミット**

```bash
git add crates/alto-wasm packages/alto-sdk tests/e2e/image-version-tests.js
git commit -m "SDK: open a thin .alto with its history store"
```

### Task 6: 復旧

**Files:**
- Modify: `tests/e2e/crash-tests.js`
- Test: 同じ

**Interfaces:**
- Consumes: Task 4〜5。コードの変更は要らない見込み（薄い `.alto` で開いた文書は `LazyObjects` を持つので、チェックポイントは今の on-demand と同じく `lazy-objects.json` を書く）。試験が失敗したら、`recovery_checkpoint` の扱いを直す。

- [ ] **Step 1: クラッシュ試験に1点加える**：薄い `.alto` を置き場ありで開き、1筆描いて journal の待ち点でタブを終わらせる。復旧した文書は、描いた筆が残り、`versions.log()` が同じで、`attachFolder(store)` の後に古い版へ戻れる。

- [ ] **Step 2: 動かす**

Run: `node scripts/verify-browser.mjs --suite=crash`
Expected: PASS（新しい点も含め全て）

- [ ] **Step 3: コミット**

```bash
git add tests/e2e/crash-tests.js
git commit -m "Crash test: recover a document opened from a thin .alto"
```

PR 3 の前に、全てのブラウザ試験（`node scripts/verify-browser.mjs`）と `cargo test --workspace` を通す。

---

## PR 4：計測と記録（Task 7）

### Task 7: 長時間試験と記録

**Files:**
- Modify: `tests/e2e/soak.js`（`ALTO_SOAK_THIN=1` で `doc.save(target, { history: { folder } })` を使う。folder は OPFS の `OpfsFolderProvider`）
- Modify: `scripts/verify-browser.mjs:222-223`（`thin: process.env.ALTO_SOAK_THIN === "1"` を渡す）
- Modify: `doc/verification/ENGINE-HARDENING-RESULTS.md`（新しい節）、`doc/design/2026-11-16-versions-outside-alto.md`（状態を「実装済み」に）、`doc/MILESTONES.md` の上限の記述

- [ ] **Step 1:** 5分で動かし、サンプルに薄い `.alto` の大きさと置き場の大きさが別々に出ることを確かめる：`ALTO_SOAK_MINUTES=5 ALTO_SOAK_THIN=1 node scripts/verify-browser.mjs --suite=soak`
- [ ] **Step 2:** F-2 と同じ seed で4時間動かす（バックグラウンド、`caffeinate -dims`）：`ALTO_SOAK_SEED=1 ALTO_SOAK_THIN=1 caffeinate -dims node scripts/verify-browser.mjs --suite=soak`。合格は、240分まで保存が失敗しないこと、薄い `.alto` の大きさが版の数に比例しないこと（最後の1時間の傾きが作業文書の大きさの変化の範囲）。全てのサンプルを `verification/out/thin-soak-240/` に残す。WASM メモリ、JS ヒープ、GPU の計上は分けて書く。
- [ ] **Step 3:** 結果を記録し、コミットする。

```bash
git add tests/e2e/soak.js scripts/verify-browser.mjs doc
git commit -m "Record the thin .alto soak"
```
