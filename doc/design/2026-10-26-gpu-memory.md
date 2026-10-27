# GPU メモリの内訳と制御

作成日：2026-10-26
状態：実装済み。実装と検証の詳細は [GPU-MEMORY-RESULTS-2026-10-26.md](../verification/GPU-MEMORY-RESULTS-2026-10-26.md)。以下の原設計に対する具体化・制約は末尾の「実装上の決定」を参照。

対象は `crates/alto-gpu` のテクスチャ予約、M5 の中間ターゲット、表示タイルの退避、予算に対する確保量、スロットの再利用である。B6（[doc/verification/EMBEDDING-API-RESULTS-2026-10-25.md](../verification/EMBEDDING-API-RESULTS-2026-10-25.md)）が Metal で GPU 484.8 / 512 MiB と報告している。内訳を見えるようにし、小さい予算での過大予約と効果付き文書の中間確保を減らす。既定の 512 MiB における B6 の削減量は、下記の正規データ保持条件を満たす別方式を検証するまで未確定とする。

## コードで確かめた内訳

`FrameReport.gpu_bytes` はブラウザの実割り当てではない。`frame_report`（`crates/alto-gpu/src/renderer.rs` 2154–2170 行）が、エンジン自身の式で足した 1 個の `u64` である。予算 512 MiB、デバイスの配列層数が 512 以上のとき、描画器の生成（同 635–703 行）は文書と無関係に次を確保する。

| 資源 | 大きさ | バイト |
|---|---|---|
| 内容アトラス `content atlas` | 256×256 RGBA8 × 512 層 | 128 MiB |
| 表示タイル `display tiles` | 256×256 RGBA8 × 256 層 | 64 MiB |
| ピラミッド用 scratch `pyramid input` | 512×512 RGBA8 × 1 | 1 MiB |
| 合計（固定） | | **193 MiB** |

層数の式は `max_texture_array_layers.min(512).min(((budget_mb.max(128) - 64) * 4).max(64))`（635–639 行）。予算 512 では 512 層で頭打ちになる。表示タイルの 256 層と scratch は予算で変わらない。1 層は `256×256×4 = 262144` バイト。scratch は `SCRATCH_BYTES`（24–26 行）。

B6 のピーク 484.75 MiB（表の 484.8 は小数 1 桁への丸め）は、この 193 MiB にストローク 389 枚 × 786432 バイト = 291.75 MiB を足した値と一致する。786432 は出力 RGBA8、下地 RGBA8、カバレッジバッファ 256×256（`renderer.rs` 2278–2296 行、計上は 2164–2168 行）の和である。

389 枚は B6 の全面ブラシそのものである。`tests/e2e/browser-tests.js` 1041–1050 行が、4K（3840×2160）に半径 1024 のダブを打つ。半径は `size * 0.5 * pressure`（`crates/alto-raster/src/brush.rs` 57–62 行）。`size` 2048、`spacing` 0.15、筆圧 1、`pressure_size` 既定 true。入力 40 点からダブ 110 個、タイル 389 枚。うちキャンバスに交わるのは 135 枚（横 15 × 縦 9、101.25 MiB）で、外が 254 枚（190.5 MiB）である。`stamp`（`renderer.rs` 2227–2241 行）はキャンバスで切らない。

このシーンは半透明ラスタ 20 枚だけで、効果もベクターも無い（`browser-tests.js` 933–951 行）。M5 とベクターの計上は 0 である。ピークを拾うのはストロークが生きている `flush` の直後（1050 行）で、`end` の後は固定の 193 MiB に戻る。484.8 は「20 レイヤーの文書の最低必要量」ではない。空の予約 193 MiB と、キャンバス外を含む描画中タイル 291.75 MiB である。

M5 の中間は `Rgba16Float`（1 テクセル 8 バイト）で、ROI の幅か高さが変わると作り直す（`crates/alto-gpu/src/renderer/m5.rs` 245–278 行）。ベクターはメッシュに加え、カバレッジが `幅×高さ×5`（4x MSAA の R8 と解決後の R8、`renderer/vector.rs` 206–215 行、558–588 行）。いずれも B6 の 484.75 には入っていない。効果付きの文書ではここに乗る。

WebGPU バックエンドでは `Drop` だけでの実解放に依存しない（[wgpu #4092](https://github.com/gfx-rs/wgpu/issues/4092)）。`StrokeTile::destroy`（`renderer.rs` 211–220 行）と `Retired`（327–355 行）は送信後に `destroy()` を呼ぶ。この順序は提出済みの利用を保護する。native の `wgpu::Texture::destroy()` も最終 submission に基づいて実破棄を遅延する（[wgpu-core 30.0.1 の実装](https://docs.rs/wgpu-core/30.0.1/src/wgpu_core/resource.rs.html#1683-1687)）。CPU が GPU 完了を確認することを、すべての破棄・再利用の前提にはしない。

## 制約

- 画素が変わる変更（中間を 8 ビットにする、MSAA の段数、縮小の式）は範囲外。入れるなら描画バージョンを新しくし、ハッシュの回帰を付ける。この設計は画素をビット単位で保つ。文書の `format_version`（`crates/alto-doc/src/lib.rs` 21–40 行）はファイル形式で、描画バージョンではない。描画バージョンは、画素を変える変更を入れるとき初めて足す。
- 正規の描画データは現在のキャンバス境界で削除しない。現在見えないタイルも、後のレイヤー移動で見える。表示用 GPU 資源の削減と、正規タイルの生成・保存は分けて設計する。
- CI は SwiftShader（`scripts/verify-browser.mjs`）。性能とメモリの合否に Metal の絶対値は使わない。main との相対差か、エンジンの計上値で見る。SwiftShader のベンチは「GPU budget too small for one display tile」で落ちる既存の制約があり、B6 のゲートは Metal の手動計測のままにする。
- 別の進行中の変更が、GPU と CPU の画素一致テストの既存本体、GPU 待ちの上限（`packages/alto-sdk/src/worker.ts` の `presentSettled`、50 ms）、README、LICENSE に触れている。実装でもそこを同時に書き換えない。ぼかしの再利用を見るテストは、既存テストの隣に新しい関数として足す。

## 1. 計測を先に分ける

### 問題

`gpu_bytes` が 1 個なので、予約が一杯なのか、文書が今そのバイトを必要としているのかが区別できない。B6 はサンプルの最大値だけを残す（`browser-tests.js` 1075–1088 行）。効果のピークとストロークのピークが別フレームでも、1 個の最大値には出ない。

予算判定の式も揃っていない。`frame_report` と `stamp`（2247–2252 行）は scratch の 1 MiB を固定に入れる。M5 の `alloc`（`m5.rs` 237–238 行）は入れない。

### 設計

初期導入では4区分の和を現行の `gpu_bytes` と一致させる。5個目の `live_fixed` は和に入れない。5節で退役台帳を加えた後は、未解放分も元の区分に一度だけ計上する。その段階で現行式より上限が増える差は、別の計上改善として記録する。

```rust
/// Managed reserved/allocated byte estimates, not total driver VRAM.
/// The initial rollout matches today's `gpu_bytes`; section 5 adds retired bytes.
/// `live_fixed` is the occupied part of `fixed` and is not added again.
#[derive(Default, Clone, Copy, Debug)]
pub struct GpuBytes {
    /// Atlas capacity + display-cache capacity + the 512×512 scratch,
    /// whether or not a slot holds a tile.
    pub fixed: u64,
    /// Occupied atlas slots + occupied display slots + scratch.
    /// With retired fixed allocations: empty = fixed - live_fixed - retired_fixed.
    pub live_fixed: u64,
    /// M5 pool, physical size (bucket, not the logical ROI). Includes the 1×1 dummy.
    pub effects: u64,
    /// In-progress stroke tiles: output + base + coverage, plus a selection texture.
    pub stroke: u64,
    /// Vector meshes + coverage (`w*h*5`) + glyph atlas. Meshes already evict.
    pub vector: u64,
}

impl GpuBytes {
    pub fn total(self) -> u64 {
        self.fixed + self.effects + self.stroke + self.vector
    }
}
```

`FrameReport`（`renderer.rs` 85–94 行）は `gpu_bytes: u64` を残し、その値は `gpu.total()` にする。隣に `pub gpu: GpuBytes` を足す。呼び出し側と B6 の `f.gpuBytes` は合計のまま動く。

区分の中身は次のとおり。

- `fixed`：`atlas_slots.len() * 262144 + display_layers * 262144 + SCRATCH_BYTES`。表示層数は今は定数 256。予算で変えたあとは実際の層数を使う。
- `live_fixed`：`atlas_map.len()` と、`cache_slots` の `Some` の数。scratch は常に足す。
- `effects`：`Resources::bytes`（`m5.rs` 116–118 行）。バケット化のあとは物理の幅と高さで数える。論理 ROI で数えると、余白の分だけ予算を下回って見える。
- `stroke`：今の 1 枚 786432、選択があるとき +262144。
- `vector`：`VectorResources::bytes`（`vector.rs` 206–216 行）。

`stamp` と M5 の `alloc` は、この同じ関数で固定分を計算する。scratch の抜けをなくす。

WASM は `FRAME_FIELDS`（`crates/alto-wasm/src/lib.rs` 13–47 行）の末尾に名前を足す。`tick`（388–392 行）が `gpu_bytes` の直後ではなく末尾に数値を足すので、既存の並びは動かない。`debug_assert_eq!(out.len(), FRAME_FIELDS.len())`（426 行）がずれを拾う。

```text
"gpu.fixed"
"gpu.liveFixed"
"gpu.effects"
"gpu.stroke"
"gpu.vector"
```

Worker の `decodeFrame`（`packages/alto-sdk/src/worker.ts` 432–454 行）は、ドット付きの名前を `profile` 以外すべて `prefetch` に入れる。`gpu.*` を `prefetch` に落とさないよう、`gpu` グループを足す。`presentSettled`（同 19–22 行、50 ms）は触らない。

フレームの形は次のとおり。`gpuBytes` は合計のまま残す。

```ts
interface FrameStats {
  gpuBytes: number;
  gpu: {
    fixed: number;
    liveFixed: number;
    effects: number;
    stroke: number;
    vector: number;
  };
}
```

型は `packages/alto-sdk/src/protocol.ts` の `FrameStats`（47–56 行）に書く。`unstable.ts` の `FrameStats` は `Record<string, unknown>` なので、診断の `settle()` にはそのまま載る。公開の組み込み API には出さない。

B6 は合計の最大値に加え、その最大値を出したフレームの `gpu` を残す。区分ごとの最大を別々に足さない（別フレームの合計になり、ピークを超えて見える）。ストロークの `end` のあとにも 1 フレーム記録し、待機中が固定だけであることを表に残す。結果は `verification/` に日付付きで書き、表は次の形にする。

| 時点 | fixed | liveFixed | effects | stroke | vector | 合計 |
|---|---|---|---|---|---|---|
| ストローク終了後 | 193 MiB | （占有分） | 0 | 0 | 0 | 193 MiB |
| B6 ピーク（全面ブラシの flush 直後） | 193 MiB | （占有分） | 0 | 291.75 MiB | 0 | 484.75 MiB |

Playground の 1 行表示（`examples/browser-playground/main.js` 414 行）も、合計の横に 4 区分を出す。

### キャンバス外の正規データを保持する

B6 のキャンバス外 254 枚・190.5 MiB は削減候補の調査対象であり、そのまま不要と判断できるメモリではない。`finish_stroke` → `endBrush` は、そのタイルも `Op::SetTiles` としてレイヤーに保存する（`crates/alto-wasm/src/lib.rs` 670–719 行）。表示シェーダが現在のキャンバス外を描かないことと、文書がその画素を必要としないことは別である。

現行 head `f0d1a9` の配布 WASM を Chrome 155 / Apple Metal で確認した。64×64 の文書でブラシを `(32,32)` から `(-384,32)` まで動かすと、正規タイル `(-2,0)` が残る。レイヤーを右へ 416 px 移動すると、キャンバス内の `(32,32)` は `[255,0,0,255]` になる。元のキャンバスと全く交わらないタイルが、編集後には表示に必要となる例である。

したがって、この計画では `stamp` にキャンバス交差判定を入れて正規タイルの生成を省かない。B6 の 389 枚を 135 枚だけにする変更と、「画素を変えずに 190.5 MiB 削減する」という採用見積もりは外す。既定の予算では、この B6 のピークが 484.75 MiB のままでも、この計画の実装失敗とはしない。

表示用 GPU 常駐量だけを減らす後続案には、例えば、元の下地・選択・変形・ダブ順序を保持し、キャンバス外も同じ GPU カーネルで小さいバッチに分けて生成・読み戻す方式がある。ただし、この PR ではその方式を採用しない。正規タイルのハッシュ、ストローク確定の原子性、WASM 側の保持量、確定時間と退役待ちを含む GPU ピークを別途検証してから設計する。CPU 方式へ移す場合も、既存カーネルとの画素一致が条件になる。別方式で保持したデータを失ったまま GPU 常駐分だけを計上から消してはならない。

### 触るファイル

- `crates/alto-gpu/src/renderer.rs`：`GpuBytes`、`FrameReport`、`frame_report`、`stamp` の予算式。正規ストロークタイルの生成範囲は変えない。
- `crates/alto-gpu/src/renderer/m5.rs`：`alloc` の固定分を同じ式にする。`bytes` は物理サイズ。
- `crates/alto-gpu/src/renderer/vector.rs`：`bytes` を `vector` 区分へ。計上以外は変えない。
- `crates/alto-wasm/src/lib.rs`：`FRAME_FIELDS` と `tick`。`brush_input` / `endBrush` の正規データ生成は現行のまま。
- `packages/alto-sdk/src/worker.ts`：`decodeFrame` だけ。`packages/alto-sdk/src/protocol.ts`：`FrameStats`。
- `tests/e2e/browser-tests.js`：B6 の記録。ゲートは合計 ≤ 512 MiB のまま。
- `examples/browser-playground/main.js`：表示。

### リスク

- `gpu_bytes` の意味を合計のままにしておかないと、B6 の比較が切れる。
- 現在のキャンバスと交わらないという理由で正規タイルを捨てると、後のレイヤー移動で画素が欠ける。ストロークの全変更キーと保存内容を保持する。
- `live_fixed` を合計に足すと二重計上になる。和は 4 区分だけ。

### テスト

- ホストの単体テストで、空の描画器相当の数（アトラス層、表示層、scratch）から `total()` が今の式と一致すること。scratch を M5 側の式にも含むこと。
- 64×64 の文書で `(32,32)` から `(-384,32)` へ描き、正規タイル `(-2,0)` を保持すること。レイヤーを右へ 416 px 動かした後の `(32,32)` が `[255,0,0,255]` になること。境界に触れるだけのタイルに依存しない再現条件とする。
- 同じ描画・移動を main と比較し、全正規タイルのキーとハッシュを一致させること。変形、選択、Undo/Redo、保存・再読込を挟んでも、後から見える画素が一致すること。
- SwiftShader の既存機能テストが画素を変えないこと。Metal の B6 は、ピークのフレームの 4 区分と、終了後のフレームを結果の表に書く。CI は絶対値の 484.8 で合否を見ない。

## 2. M5 中間テクスチャのプール

### 問題

プールはキュー順の配列で、同じ添字かつ同じ幅と高さのときだけ再利用する（`m5.rs` 2 行、72–80 行、197–280 行）。1 枚の表示タイルの中で `next` は単調に増える（1048 行で 1 から。0 番は 1×1 の透明入力）。寿命が重ならない中間同士は共有しない。タイルの終わりで `next` 以降を `drain` して捨てる（1101–1103 行）ので、次のタイルが小さければ高水位のテクスチャは残らない。

サイズが 1 テクセルでも違えば作り直し、古い方は `Retired` に渡す（275–277 行）。ぼかしは ROI を広げる（`expand`、608–629 行、695–708 行）ので、タイルごとにサイズが変わり、確保と破棄が繰り返される。

予算超過の `pool.truncate`（226 行）は `destroy()` を通さない。`Pooled` に `Drop` は無く、WebGPU ではテクスチャが残る。

### 設計

キーはフォーマットと、64 の倍数に切り上げた幅と高さである。64 はワークグループ 16 の倍数で、256 のタイルはそのまま入る。2 の冪まで上げない（280 が 512 になると面積が約 3 倍になる。64 なら 320 で、面積は約 1.3 倍）。

```rust
fn bucket(n: u32) -> u32 {
    n.max(1).next_multiple_of(64)
}
```

1×1 の透明入力はバケットに入れない。ぼかしがそれと実ターゲットを共有すると、前の中身を読む。

フレームをまたいで保持し、未使用が `POOL_IDLE_FRAMES`（8）に達したら `Retired` へ渡す。タイル毎の `drain` はやめる。全 consumer の submit 後に `destroy()` し、実解放の計上は 5 節の退役台帳で追う。`truncate` で `Drop` に任せない。

寿命が重ならない中間は、同じ物理テクスチャを共有する。レンダーグラフのエイリアシング（[Render Graph Aliasing](https://matthewberger.dev/nightshade-book/render-graph-aliasing.html)）と同じ考え方である。今のコードは即時モードなので、グラフを二度歩くのではなく、最後の読み出しをエンコードし終えた `Target` をフリーリストへ返す。1 つのコマンドエンコーダの中では順序が保たれる。占有者 A の最後の読み出し、全体 clear、占有者 B の書き込みを、この順で GPU コマンドとして記録する。同じ pass の入出力へ同一 texture を束ねない。`queue.write_texture` などの queue 書き込みをこの並びへ挟むことはできず、その場合の未提出参照は 5 節の pin で保護する。

同時に生きる枚数は、1 回のディスパッチが束ねる枚数と、まだ読み終えていない祖先である。ノードの数だけ `Rgba16Float` を持たない。シェーダは入力 3 枚と出力 1 枚を束ねる（`effects.wgsl` 4–7 行）。

**再利用時は、消すことと、読み範囲を論理 ROI に限ることの両方を行う。**

消すだけにしない理由。ぼかし（モード 2、`effects.wgsl` 31–34 行）と双線形 `a_linear`（13 行）は、書き込んだ矩形の外を読む。`load_a` / `a_at` / `load_b`（11–14 行）は論理の幅と高さではなく `textureDimensions` と比べている。物理が論理より大きいと、余白の古いテクセルがカーネルに入る。消し忘れが一箇所でもあると、前のフレームの画素がぼかしに混ざる。

読み範囲を限るだけにしない理由。`textureLoad(clip_base, vec2i(id.xy))`（28 行、40 行、45 行）のように、呼び出し ID で直接読む箇所がある。ディスパッチを論理サイズのままにしておけば、この経路は書き込み済みの範囲に収まる。将来足す読み出しが `textureDimensions` を見ると、再び余白を読む。余白が透明であれば、その読み出しは今の「範囲外は 0」と一致する。

具体的には次の二つである。

1. **読み範囲。** `Params` に論理サイズ `sizes: [u32; 4]`（A の幅と高さ、B の幅と高さ）を足す。`load_a`、`a_at`、`load_b` は `textureDimensions` ではなく `sizes` と比べ、外は `vec4f(0)` を返す。`a_linear` が隣を読む 1 テクセルも、論理サイズの外なら 0 である。ディスパッチは今どおり論理の `roi.w` / `roi.h`（`m5.rs` 360 行）で、物理のバケットサイズでは起動しない。物理と論理が等しいとき、境界は今の `textureDimensions` と一致するので、画素は変わらない。
2. **再利用のたびに消す。** その占有者の最初の書き込みの前に、物理テクスチャ全体を透明にする。空のレンダーパスの `LoadOp::Clear` を使う。ピラミッドの scratch が既にそうしている（`renderer.rs` 2735–2751 行）。`clear_texture` は wgpu の WebGPU バックエンドに無く、使わない。M5 のターゲットは既に `RENDER_ATTACHMENT` を持っている（`m5.rs` 264 行）。同じフレームで別の論理 ROI に付け替えるときも、付け替えの前に全体を消す。余白だけ消すと、新しい書き込みが覆わない内側に前の占有者が残る。

`bytes()` と予算は物理サイズで見る。余白は実際に確保されている。

### 触るファイル

- `crates/alto-gpu/src/renderer/m5.rs`：`Pooled`、`alloc`、`compose_m5` の `drain`、`bytes`。
- `crates/alto-gpu/shaders/effects.wgsl`：`load_a`、`a_at`、`load_b` と `Config`。
- `crates/alto-gpu/src/renderer.rs`：`Retired` に載せるタイミング（5 節）。

### 削減の見込み

B6 には効果が無いので、この節は 484.75 を動かさない。効果付きの文書で、同時に持つ `Rgba16Float` がグラフのノード数から、同時に生きている枚数へ減る。1 枚 256×256 は 512 KiB。半径 12 で 280 に広がった ROI は、バケット 320 で面積が約 1.3 倍になる。その余白が、サイズが近い次のタイルでの作り直しをなくす代わりである。

### リスク

- 消し忘れ、またはクランプの抜けが、前の利用の画素をぼかしへ混ぜる。失敗は文書の内容に依存し、再現がフレーム順になる。
- バケットの余白を論理サイズで計上すると、予算を超えてから気づく。
- 1×1 のダミーをプールに混ぜると、透明であるべき入力が前の画像になる。

### テスト

画素の契約は、GPU の読み戻しが CPU のぼかしと一致することである。既存の「Gaussian blur matches CPU reference across both tile seams and preserves source」（`tests/e2e/m5-tests.js` 294 行）の本体は書き換えない。隣に新しいテストを足す。

1. 大きい白い矩形と大きい半径で、バケットを非ゼロで満たす。
2. 続けて小さい矩形と小さい半径を描く。物理テクスチャは前のものを再利用する。
3. 小さいぼかしのハローを、CPU の `intervalBlur`（同ファイル 51–58 行）と全画素で比べる。余白から古い白が漏れると、ハローのアルファが 0 にならない。
4. 小さい方を連続で 2 回描き、2 回目が確実に再利用になるようにする。

物理サイズと論理サイズが等しい既存の継ぎ目テストは、クランプが今の境界と一致することの回帰として残す。ホストの単体テストで、`bucket` と「ディスパッチサイズが論理サイズであること」を見る。

このテストは契約（古いテクセルを読まないこと）を見る。消すこととクランプの片方だけが残っていても、画素は契約を満たし得る。両方を実装に残し、シェーダが `sizes` を見ていることは単体テストで固定する。

## 3. 表示タイルと内容タイルの退避順

### 問題

表示スロットの退避（`cache_slot`、`renderer.rs` 1375–1410 行）は、今見えていないタイルから、先読み済みを先に、次に古い `used` の順で選ぶ。ビューポートからの距離も、解像度が今のズームに合っているかも見ない。最粗の段も、視界から外れると退避される。

急にズームアウトしたとき、現在の段が未完成でも、より粗いキャッシュを先に描く経路がある（2030–2037 行、レベルが深い順）。その最粗が無いと、ズームの途中が穴になる。

内容アトラスは、合成中にピンした層以外を `used` の LRU で空ける（`free_atlas_slot`、1026–1050 行）。距離は見ない。

### 設計

表示タイルを 3 つの箱に分ける。Chromium の TileManager が、今必要なタイル、すぐ必要になるタイル、その先のタイルを分けて、遠いものから捨てるのと同じ分け方である。

| 箱 | 中身 | 退避 |
|---|---|---|
| 今 | `visible_keys` と、ピンした最粗 | しない |
| すぐ | ビューポートの外 1 画面以内の、理想レベル。および `prefetch_keys` が既に計算する 1 段粗いタイル（1569–1578 行） | いずれかの次 |
| いずれか | 残り | 先 |

箱の中では、(1) 理想レベルでない段を先に、(2) ビューポート中心からの距離が遠いものを先に、(3) `used` が古いものを先に捨てる。距離は可視タイルのソート（1935–1941 行）と同じ式を使う。ピラミッドの親を作っている最中の 4 枚の子は、今の除外（1382–1388 行）を残す。

**最粗の段はピンし、退避しない。**

最粗のレベルは、キャンバスの長辺を 1 枚が覆う最小の段である。上限は今のクランプと同じ 12。

```rust
fn overview_level(canvas_w: u32, canvas_h: u32) -> i32 {
    let tiles = (canvas_w.max(canvas_h).max(1) as f64 / 256.0).log2().ceil();
    tiles.clamp(0.0, 12.0) as i32
}
```

4K（3840×2160）は `ceil(log2(15)) = 4`、1 枚のスパンは 4096 で、タイルは 1 枚である。長辺が `256×2^12 = 1,048,576` px 以下なら、この段は常に 1 枚である。`DocumentMeta::validate`（`alto-doc/src/lib.rs` 33–48 行）は辺の上限を持たない。それを超えるキャンバスはレベル 12 が複数枚になるので、キャンバスと交わるレベル 12 を全部ピンする。ピンした枚数が表示層数を超えるときは、最粗を捨てずに、今と同じ予算エラーを返す。

メモリの費用は 1 枚あたり 256×256×4 = 256 KiB である。4K では 1 枚、256 KiB。今の表示キャッシュは 256 層を先に確保しているので、ピンは確保量を増やさない。256 層のうち 1 層を、視界に使えないようにするだけである。4 節で層数を予算に合わせたあとも、費用は同じ 256 KiB で、その 1 層は予算の中に予約したままにする。

最粗がまだ無い状態でキャッシュが足りなくなったら、捨てる前にその 1 枚を作る。ズームアウトの穴を塞ぐのが、ピンの目的である。

内容アトラスにピラミッドは無い。合成中の `pinned` は今どおり退避しない。それ以外は、ビューポート中心から遠い内容タイルを、純粋な LRU より先に捨てる。最粗のピンは表示タイルだけに適用する。

退避したタイルを CPU 側で圧縮して持つ（Krita が退避タイルを圧縮して残すやり方）のは、この設計の最初の実装には入れない。表示タイルは作り直せる。残すと WASM ヒープを使い、B6 が別に見ている 1024 MiB の枠を食う。

### 触るファイル

- `crates/alto-gpu/src/renderer.rs`：`cache_slot`、`free_atlas_slot`、`visible_keys` の更新箇所（1921 行）。

### 削減の見込み

配列を先に確保しているあいだは、退避順は `gpu_bytes` を減らさない。減るのは、4 節で層数が予算より小さくなったあと、どのタイルを残すかの選択である。ピンの費用は 4K で 256 KiB、1 スロットである。

### リスク

- 最粗の生成を、見えているタイルの合成より先に走らせると、コールド表示が遅くなる。圧力がかかったときだけ作り、毎フレームは作らない。
- 「すぐ」の範囲を広げすぎると、「今」が空スロットを取れなくなる。外 1 画面に固定する。

### テスト

- 256 層を埋めたあと、視界のタイルと最粗が残り、理想でない段が先に捨てられること。
- 最粗をピンした状態でズームアウトし、理想の段が未完成でも穴が無いこと（2030 行の粗いタイル描画）。
- 回転と、キャンバス端で、ピンされる枚数が定義どおりであること。4K なら 1 枚。
- 既存の「表示キャッシュを埋めてから置換する」機能テストが、退避後も画素一致であること。

## 4. 予算に合わせた固定の確保

### 問題

固定の 193 MiB は、予算 512 MiB では文書が空でも払う。予算を下げても表示の 64 MiB と scratch の 1 MiB は減らない。アトラスが 512 層を下回るのは、予算が 192 MiB 未満のときだけである（635–639 行。`(budget - 64) * 4` が 512 を切るのが 192 未満）。

予算 128 MiB は `budget_mb.max(128)` で 128 に切り上げられ、アトラス 256 層（64 MiB）+ 表示 64 MiB + scratch 1 MiB = 129 MiB になり、予算を確保の時点で超える。`budget_bytes` も同じ切り上げ（787 行）である。

### 設計

要求された予算を 64 MiB 未満は拒否する。64 以上は切り上げない。固定の上限を予算の半分にし、残りをストローク、効果、ベクターに残す。scratch の 1 MiB は 2×2 の子を直列で受ける既存の 1 枚のまま、半分の中から先に引く。残りをアトラス 2、表示 1 で分ける。内容タイルは複数の表示タイルから参照されるので、アトラスを厚くする。

```rust
fn cache_layers(budget_bytes: u64, max_array_layers: u32) -> (u32, u32) {
    let tile = 262_144u64;
    let scratch = 512 * 512 * 4;
    let rest = (budget_bytes / 2).saturating_sub(scratch);
    let atlas_cap = max_array_layers.min(512) as u64;
    let atlas = ((rest * 2 / 3) / tile).clamp(64, atlas_cap);
    let display_cap = max_array_layers.min(256) as u64;
    let display = ((rest - atlas * tile) / tile).clamp(32, display_cap);
    (atlas as u32, display as u32)
}
```

| 予算 | アトラス | 表示 | scratch | 固定の合計 | 今 |
|---|---|---|---|---|---|
| 512 MiB | 512 層（128 MiB） | 256 層（64 MiB） | 1 MiB | **193 MiB** | 193 MiB |
| 256 MiB | 338 層（84.5 MiB） | 170 層（42.5 MiB） | 1 MiB | **128 MiB** | 193 MiB |
| 128 MiB | 168 層（42 MiB） | 84 層（21 MiB） | 1 MiB | **64 MiB** | 129 MiB |

512 では今と同じ 193 MiB なので、B6 の固定分は変わらない。256 では固定が 193 から 128 へ、128 では予算を超えていた 129 が 64 になる。下限の 64 層と 32 層は、合計約 25 MiB で、予算 64 MiB の半分（32 MiB）に収まる。

デバイスの `max_texture_array_layers` は両方の上限にかける。`request_device` の `limits`（640–642 行）は、今は `slots.max(256)` である。これを `max(atlas, display)` にする。

視界が確保した表示層数を超えたら、今の「256 枚を超える」エラー（1916–1919 行）を、実際の層数で出す。層を後から増やして予算を超えない。

`fixed` の計上はこの層数を使う。未解放の固定配列が無いとき、空の層は `fixed - live_fixed` に出る。切替時の退役分は5節の `retired_fixed` も引く。

### 触るファイル

- `crates/alto-gpu/src/renderer.rs`：`Renderer::new` の層数、`cache_slots` の長さ（765 行）、`frame_report` の `+ 256`。

### 削減の見込み

既定の 512 MiB では固定は 193 MiB のままで、この B6 に対する削減は約束しない。1 節のキャンバス外の正規データを保持した別方式が未設計・未検証のためである。この節は、小さい予算で固定が予算の半分を超えないようにする。128 MiB では約 65 MiB 減り、予算内に収まる。

### リスク

- 表示層を 256 未満にすると、広いビューポートで予算エラーが増える。エラーは今と同じ種類で、黙ってタイルを欠かさない。
- アトラスを薄くしすぎると、20 レイヤーの合成が毎タイルで追い出しになる。2:1 は内容を厚くする側に倒してある。計測の `atlas_evictions`（`RenderProfile`、108 行）で見て、半分の配分は変えないまま、比率だけを結果で見直す。

### テスト

- 上の 3 行の表を、GPU 無しの単体テストで固定する。
- 64 MiB 未満の予算が拒否されること。128 MiB で `fixed` が予算以下であること。
- 512 MiB では層数が今と同じ 512 と 256 であること。
- SwiftShader で、小さいキャンバスの機能テストが通ること。広いビューポートが層数を超えたとき、既存の予算エラーになること。

## 5. submit 境界と未提出の参照を区別する

### 契約と問題

この描画器は、同じ device の同じ queue に処理を提出する。`submit(A)` で旧内容を読み、その後に `write_texture(B)` と `submit(C)` で新内容を使うと、旧読み出しと後続書き込みの依存は queue 順で保たれる。CPU が `on_submitted_work_done` を観測するまで上書きを止める必要はない（[wgpu の説明](https://github.com/gfx-rs/wgpu/discussions/3458)）。前の present が飛行中であることだけを、後の submit のスロット再利用禁止理由にしない。

危険なのは、encoder に旧内容を読むコマンドを記録したが、まだ submit していない場合である。その間の `queue.write_texture` / `write_buffer` は次の submit のコマンドより先に実行され、旧内容を上書きし得る。GPU 完了番号が 0 のままでも、この未提出参照は存在する。完了番号だけで再利用を判定してはならない。

同じ encoder の内部では、A の最終読み出し → clear → B の書き込み、という GPU コマンドの順序で物理 texture を共有できる。これは 2 節の M5 プールに適用する。各 pass の入出力が同時に alias しないことと、A の後続 consumer が残っていないことが条件である。

`wgpu::Texture::destroy()` と、生の native / HAL handle の直接破棄も区別する。このコードが使う高水準の `wgpu` API は、提出済みの最終利用まで実体を保持する。WebGPU でも、全 consumer の submit 後なら明示的な `destroy()` を呼べる。未提出の consumer が残る間は呼ばない。

### 設計

再利用可否は、次の三つに分ける。

| 状態 | 再利用・破棄の条件 |
|---|---|
| 未提出の encoder が旧内容を参照する | queue 書き込みによる再利用と `destroy()` を禁止する。参照を pin し、その encoder の submit または破棄まで保持する |
| encoder 内で旧内容の最後の読み出しを記録済み | 後続の clear と新しい書き込みを同じ encoder に記録して再利用できる。CPU の queue 書き込みは使わない |
| 旧内容の全 consumer を提出済み | 同じ queue に後続の書き込み・submit を並べて再利用できる。CPU の完了確認は不要。用途を終えたオブジェクトは `destroy()` できる |

`cache_slot` と `free_atlas_slot` は、この未提出 pin と既存の可視・最粗・合成中の pin を守る。完了待ちの callback がまだ走っていないという理由で、提出済みの層を再利用不可にしない。空いた層を同じ queue で上書きするときに `destroy()` は呼ばない。

`Retired` は、未提出の consumer を持つ texture / buffer を保持し、**全 consumer の submit 後に** `destroy()` する。実体が GPU 上で保持されている間は、退役台帳に分類・推定バイト数・最終 submission を残す。新しい資源の確保に使う予算は、この未解放上限も含める。実装の初期段階では、1 節の4区分の和を現行式と一致させ、その後に未解放分を元の区分へ一度だけ加える。固定配列を作り替えた際の未解放分は `retired_fixed` として区別し、その期間の空予約は `fixed - live_fixed - retired_fixed` とする。退役済みの実体を空の層として表示せず、active と退役で二重計上もしない。

送信番号と完了番号は、この台帳の回収・上限計上のために使う。完了 callback は描画器を借用せず、device epoch ごとの完了番号を単調に進める。同じ queue 上の再利用を一律に止める fence には使わない。

```rust
struct GpuTimeline {
    next: u64,
    completed: Arc<AtomicU64>, // retirement accounting, not a reuse fence
}
```

native で callback の進行が必要なら `device.poll(Poll)` を使う。WebGPU ではイベントループへ戻して回収する。追加の GPU 完了待ちや `poll(Wait)` を通常の再利用経路に入れない。`Blocked` / 遅延は、未提出 pin のために使える層が無い場合か、未解放分を含めると新規確保の予算が足りない場合に限る。既存の present pacing と Worker の 50 ms 制限は変えない。

CPU への readback / map は従来どおり対応する完了を待つ。他 queue や外部 API の所有権移譲を将来扱う場合も、それぞれの同期契約が必要になる。この設計はその保証を追加しない。

デバイス喪失時は、その device epoch の資源と台帳を破棄して作り直す。失われた queue の完了通知を待ち続けず、旧 epoch の callback が新しい epoch の完了番号を進めないよう、完了状態も epoch ごとに分ける。

### 触るファイル

- `crates/alto-gpu/src/renderer.rs`：`Retired`、未提出 pin、`submit_composite`、`upload_timed`、`cache_slot`、退役バイト数の計上。
- `crates/alto-gpu/src/renderer/m5.rs`：Target の最後の参照と、encoder 内再利用の clear / 書き込み順序。
- `crates/alto-gpu/src/renderer/vector.rs`：カバレッジの退役を consumer の submit 境界へ結び付ける。

`submittedWorkDone`（`alto-wasm/src/lib.rs` 1127–1137 行）と Worker の 50 ms 制限は別の変更の対象である。この計画からは変更せず、台帳は描画器の既存 queue を追跡する。

### 削減の見込みとリスク

この節だけで B6 のバイト数が減るとは約束しない。未解放分の計上によって、報告する上限が従来値より大きくなる場合もある。目的は、未提出参照を壊さず、同じ queue で順序付けられる再利用に不要な停止を加えずに、予算と寿命を管理することである。

- 未提出参照を queue 書き込みで上書きすると、複数の読み出しが最後の値を見る。pin の対象と解除する submit を対応させる。
- GPU コマンドによる encoder 内再利用に queue 書き込みを混ぜると、期待した順序にならない。
- 未解放の実体を台帳から早く外すと、新規確保が予算を超える。逆に、完了待ちをすべての再利用へ適用すると、CPU / GPU の並行性を失う。

### テスト

- 未提出 encoder が A を読む間、同じ層への queue 書き込みと `destroy()` を行わないこと。submit または encoder 破棄で pin を解除すること。
- 同じ queue で「赤を書き込む → copy A を submit → 緑で上書き → copy B を submit」とし、CPU が完了を観測する前でも結果が赤・緑の順になること。全 copy の submit 後に `destroy()` しても結果が保たれること。Chrome 155 / Metal のレビュー時の再現でも、この結果を確認した。
- M5 の同じ encoder で「A を読む → 全体 clear → B を書く → B を読む」を記録し、旧 A と新 B の参照結果が一致すること。同じ pass の入出力を alias させないこと。
- native の安全な `wgpu` API でも submit 後の `destroy()` と読み戻しを検査すること。未提出の利用を destroy してよいというテストにはしない。
- 退役待ちの上限を予算に含み、完了通知後に一度だけ減算すること。CPU の完了確認が未了でも、追加確保しない同一 queue の再利用は進められること。
- デバイス喪失後の再作成が旧 callback に影響されないこと。永遠に完了しない旧 queue を理由に回復を止めないこと。

## 実装の順序

1. **計測（1 節の型と API）。** 初期導入では合計を現行の `gpu_bytes` と一致させる。キャンバス外を含む正規ストロークタイルを維持した状態で B6 の基準を記録する。
2. **submit 境界と退役台帳（5 節）。** 未提出 pin、提出済みの同一 queue の再利用、未解放上限の計上を分ける。全再利用を GPU 完了待ちにしない。
3. **予算に合わせた層数（4 節）。** 512 MiB の固定は 193 MiB のまま。128 MiB と 256 MiB の超過と過大予約をやめる。
4. **退避の箱と最粗のピン（3 節）。** 正規の内容を捨てず、作り直せる表示キャッシュの退避を選ぶ。
5. **M5 のプール（2 節）。** encoder 内の寿命と、ぼかしの画素検査が揃ってから。同じ queue の提出済み利用には完了待ちを足さない。この B6 に効果は無い。

キャンバス外ストロークの GPU 常駐量削減は、正規データ保持と確定処理を別方式で保証できてからの独立した設計とする。190.5 MiB は今後調べる内訳であり、この実装順序による達成済み・保証済みの削減量ではない。

各段階で、Rust のテスト、clippy、SwiftShader の機能テストを通す。Metal の B6 は区分付きで記録し、CI の合否には使わない。

## やらないこと

- **中間テクスチャを `Rgba16Float` から 8 ビットへ下げることはしない。** ぼかしと合成の画素が変わる。描画バージョンを上げ、ハッシュの回帰テストを付ける必要がある。描画バージョンは今は無く、この設計では足さない。
- MSAA の段数、グリフアトラスの解像度、ピラミッドの平均の式を変えること。同じ理由で画素が変わる。
- CI を Metal の絶対値（484.8 MiB など）で落とすこと。CI は SwiftShader で、合否は計上値の一致と、main に対する画素と機能の相対比較にする。
- `clear_texture`。wgpu の WebGPU バックエンドには無い。空のレンダーパスの `LoadOp::Clear` を使う。
- 退避タイルの CPU 側圧縮保持。最初の実装には入れない。WASM の 1024 MiB 枠を使うためである。
- スパーステクスチャでアトラスの空層を物理的に外すこと。このコードが使う WebGPU の範囲では、層ごとのレジデンシーは使えない。空層は `fixed - live_fixed - retired_fixed` で区別する（退役分が無ければ `fixed - live_fixed`）。
- ベクターのカバレッジを M5 のプールへ載せること。計上は `vector` に入れる。プールの共有は、M5 が安定してからの候補にする。
- 進行中の別変更が触る、GPU と CPU の画素一致テストの既存本体、`presentSettled` の 50 ms、README、LICENSE。ぼかしの再利用テストは新しい関数として隣に足す。
- 正規ストロークタイルを現在のキャンバス境界で切り捨てること。後の移動や保存再読込で必要になるデータを維持する。
- `Drop` だけに解放を任せること。全 consumer の submit 後に明示的な `destroy()` を行い、実解放の上限計上は完了通知で回収する。
- 同じ queue の提出済み処理にも CPU の GPU 完了確認を一律に要求すること。未提出 pin と退役の計上を分ける。

## 実装上の決定

- `renderer/memory.rs` が `GpuBytes`、予算別配列層数、device ごとの退役台帳を持つ。`renderer/cache.rs` は退避順位と最粗の作成を担当する。既存の MRT8 と main の present pacing を統合した。
- `gpuBytes = fixed + effects + stroke + vector`。`liveFixed`、`retiredFixed`、`retired` は非加算の部分集合。提出した一時コマンドバッファも、完了通知まではその用途（固定送出／効果／ベクター）に計上する。したがって固定配列の予約は `fixed - retiredFixed` で読み取る。スロット数、最粗の準備枚数、プールの確保回数・再利用回数も診断に追加した。
- これは管理対象の予約・描画資源の推定であり、総 VRAM・ブラウザの Surface・ドライバの作業領域・読み戻し用バッファを含む完全な物理ピークではない。B6 のサンプルも `endBrush` 内部の一時 readback ピークを観測しない。この境界を厳密な全プロセス GPU メモリ上限と解釈しない。
- アトラスの未提出 pin は既存の encoder 単位の `pinned` を維持する。再開可能ジョブの入力も既存の pin を保持する。M5 は `Arc`/`Weak` lease で `Target` の生存を追い、保持中の祖先・クリッピング下地がある間は再割当てしない。全体 clear は同じ encoder 内の GPU 命令であり、queue 書き込みを使わない。
- 退役台帳は texture の実際の寸法・format・sample count と buffer size で計上する。同一 queue への提出後に明示的 destroy し、完了 callback の単調番号で推定上限を回収する。新規確保では退役分を引き続き数える。単なるスロット・バケットの再利用には完了待ちを足さない。Renderer の再生成が device epoch の境界となる。
- 最粗は、残り空スロットが `overview 枚数 + 4` 以下になった時点、または最初の退避で作成要求を立てる。可視フレームの合成を済ませてから、Worker の先読みスライスで子を一枚ずつ作る。巨大な直接合成がアトラスを使い切らないよう、階層の子を保持して親を縮小する。完成済みの最粗と、構築中の必要な子は退避しない。**最初の圧力発生直後から最粗が必ず完成している保証はない**。初回の可視表示を止めないための選択で、完成までのズームに対する穴ゼロの保証は次の表示方針の課題とする。
- M5 は 64px バケットを保持し、フレーム開始時と合成開始時に8フレーム未使用のバケットを退役させる。画素の論理サイズは物理寸法と独立し、A/B/clip/output の境界をすべて制限する。1×1 の透明入力は再利用対象から外す。予算に入らない新規確保は既存の budget エラーとして返す。
- CPU 圧縮退避、キャンバス外の正規ストローク省略、ストローク確定の分割 readback、Worker の `presentSettled` 変更は実装範囲に入れない。
