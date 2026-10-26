# GPU メモリの内訳と制御

作成日：2026-10-26
状態：設計のみ。この PR は文書を足すだけで、コードは変えない。

対象は `crates/alto-gpu` のテクスチャ予約、M5 の中間ターゲット、表示タイルの退避、予算に対する確保量、スロットの再利用である。B6（[verification/EMBEDDING-API-RESULTS-2026-10-25.md](../../verification/EMBEDDING-API-RESULTS-2026-10-25.md)）が Metal で GPU 484.8 / 512 MiB と報告している。この数字を下げ、内訳を見えるようにする。

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

WebGPU では `Drop` が GPU オブジェクトを解放しない（[wgpu #4092](https://github.com/gfx-rs/wgpu/issues/4092)）。`StrokeTile::destroy`（`renderer.rs` 211–220 行）と `Retired`（327–355 行）が、送信の直後に `destroy()` している。

## 制約

- 画素が変わる変更（中間を 8 ビットにする、MSAA の段数、縮小の式）は範囲外。入れるなら描画バージョンを新しくし、ハッシュの回帰を付ける。この設計は画素をビット単位で保つ。文書の `format_version`（`crates/alto-doc/src/lib.rs` 21–40 行）はファイル形式で、描画バージョンではない。描画バージョンは、画素を変える変更を入れるとき初めて足す。
- CI は SwiftShader（`scripts/verify-browser.mjs`）。性能とメモリの合否に Metal の絶対値は使わない。main との相対差か、エンジンの計上値で見る。SwiftShader のベンチは「GPU budget too small for one display tile」で落ちる既存の制約があり、B6 のゲートは Metal の手動計測のままにする。
- 別の進行中の変更が、GPU と CPU の画素一致テストの既存本体、GPU 待ちの上限（`packages/alto-sdk/src/worker.ts` の `presentSettled`、50 ms）、README、LICENSE に触れている。実装でもそこを同時に書き換えない。ぼかしの再利用を見るテストは、既存テストの隣に新しい関数として足す。

## 1. 計測を先に分ける

### 問題

`gpu_bytes` が 1 個なので、予約が一杯なのか、文書が今そのバイトを必要としているのかが区別できない。B6 はサンプルの最大値だけを残す（`browser-tests.js` 1075–1088 行）。効果のピークとストロークのピークが別フレームでも、1 個の最大値には出ない。

予算判定の式も揃っていない。`frame_report` と `stamp`（2247–2252 行）は scratch の 1 MiB を固定に入れる。M5 の `alloc`（`m5.rs` 237–238 行）は入れない。

### 設計

4 区分の和が、今の `gpu_bytes` と一致する。5 個目の `live_fixed` は和に入れない。空の層が予約と最低必要量の差である。

```rust
/// Physical bytes the renderer has reserved or allocated.
/// `fixed + effects + stroke + vector` is today's `gpu_bytes`.
/// `live_fixed` is the occupied part of `fixed` and is not added again.
#[derive(Default, Clone, Copy, Debug)]
pub struct GpuBytes {
    /// Atlas capacity + display-cache capacity + the 512×512 scratch,
    /// whether or not a slot holds a tile.
    pub fixed: u64,
    /// Occupied atlas slots + occupied display slots + scratch.
    /// `fixed - live_fixed` is empty reservation.
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

### この計測で先に削るもの（ストロークをキャンバスで切る）

484.75 を動かしているのは固定予約ではなく、キャンバス外のストロークタイル 254 枚である。変形後の矩形がキャンバスと交わるタイルだけを作る。`GpuStroke.world` はレイヤーから文書への変換（`renderer.rs` 231–232 行）なので、タイル座標のまま切らない。文書座標に写してから `scene` のキャンバスと比べる。キャンバスの幅と高さは `brush_input` が既に持っている（`crates/alto-wasm/src/lib.rs` 660–662 行、選択があるときだけ `[width, height]` を渡している）。選択の有無と別に渡す。

交わりの判定は包含ではなく交差である。端のタイルは残す。合成シェーダはキャンバスの外を書かない（`renderer.rs` 1175–1176 行）。キャンバス外のプレビューはサンプルされないので、画素は変わらない。

B6 のこの配置（レイヤーの変形なし）では 190.5 MiB 減り、ピークは 294.25 MiB（固定 193 + キャンバス内 135 枚 × 0.75 MiB）になる。

### 触るファイル

- `crates/alto-gpu/src/renderer.rs`：`GpuBytes`、`FrameReport`、`frame_report`、`stamp` の予算式、タイル生成のキャンバス交差。
- `crates/alto-gpu/src/renderer/m5.rs`：`alloc` の固定分を同じ式にする。`bytes` は物理サイズ。
- `crates/alto-gpu/src/renderer/vector.rs`：`bytes` を `vector` 区分へ。計上以外は変えない。
- `crates/alto-wasm/src/lib.rs`：`FRAME_FIELDS` と `tick`。`brush_input` がキャンバス寸法を渡す。
- `packages/alto-sdk/src/worker.ts`：`decodeFrame` だけ。`packages/alto-sdk/src/protocol.ts`：`FrameStats`。
- `tests/e2e/browser-tests.js`：B6 の記録。ゲートは合計 ≤ 512 MiB のまま。
- `examples/browser-playground/main.js`：表示。

### リスク

- `gpu_bytes` の意味を合計のままにしておかないと、B6 の比較が切れる。
- レイヤーが回転しているとき、タイル座標で切るとキャンバス上の画素が欠ける。文書座標で交差を見る。
- `live_fixed` を合計に足すと二重計上になる。和は 4 区分だけ。

### テスト

- ホストの単体テストで、空の描画器相当の数（アトラス層、表示層、scratch）から `total()` が今の式と一致すること。scratch を M5 側の式にも含むこと。
- キャンバス外のダブがタイルを足さないこと、端に掛かるダブは足すこと、回転した `world` では文書座標で残ること。CPU の参照ストロークと、キャンバス内のタイルが一致すること。
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

フレームをまたいで保持し、未使用が `POOL_IDLE_FRAMES`（8）に達したら `Retired` へ渡す。タイル毎の `drain` はやめる。破棄は 5 節の送信番号が完了してから `destroy()` する。`truncate` で `Drop` に任せない。

寿命が重ならない中間は、同じ物理テクスチャを共有する。レンダーグラフのエイリアシング（[Render Graph Aliasing](https://matthewberger.dev/nightshade-book/render-graph-aliasing.html)）と同じ考え方である。今のコードは即時モードなので、グラフを二度歩くのではなく、最後の読み出しをエンコードし終えた `Target` をフリーリストへ返す。1 つのコマンドエンコーダの中では順序が保たれる。占有者 A の読み出しを全部書いてから、消して、占有者 B の書き込みを書く。

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

`fixed` の計上はこの層数を使う。空の層は `fixed - live_fixed` に出る。

### 触るファイル

- `crates/alto-gpu/src/renderer.rs`：`Renderer::new` の層数、`cache_slots` の長さ（765 行）、`frame_report` の `+ 256`。

### 削減の見込み

既定の 512 MiB では固定は 193 MiB のまま。B6 の低下は 1 節のストローク切り（約 190 MiB）が担う。この節は、小さい予算で固定が予算の半分を超えないようにする。128 MiB では約 65 MiB 減り、予算内に収まる。

### リスク

- 表示層を 256 未満にすると、広いビューポートで予算エラーが増える。エラーは今と同じ種類で、黙ってタイルを欠かさない。
- アトラスを薄くしすぎると、20 レイヤーの合成が毎タイルで追い出しになる。2:1 は内容を厚くする側に倒してある。計測の `atlas_evictions`（`RenderProfile`、108 行）で見て、半分の配分は変えないまま、比率だけを結果で見直す。

### テスト

- 上の 3 行の表を、GPU 無しの単体テストで固定する。
- 64 MiB 未満の予算が拒否されること。128 MiB で `fixed` が予算以下であること。
- 512 MiB では層数が今と同じ 512 と 256 であること。
- SwiftShader で、小さいキャンバスの機能テストが通ること。広いビューポートが層数を超えたとき、既存の予算エラーになること。

## 5. 空いたスロットを、GPU が終わってから使う

### 問題

表示層とアトラス層は破棄せず、同じ層へ上書きする。`destroy()` が送信済みの作業が終わるまで解放を遅らせる、という WebGPU の性質は、上書きには効かない。

`render` は、前のフレームの present が終わったかを見る前に、表示キャッシュへ合成を書く（1943–2028 行）。`present_in_flight`（254–257 行、2148–2151 行）は present を 1 枚に抑えるだけで、その present がサンプルしている層への書き込みは止めない。合成はタイルごとにその場で `submit` する（1553–1554 行）。完了コールバックは、ネイティブでは `poll` で走り、WebGPU ではイベントループまで走らない（2020–2021 行のコメント）。

M5 の作り直しとストロークの `destroy()` は、送信の直後に呼んでいる。WebGPU はそこで実体の解放を遅らせる。ネイティブで、使用中のテクスチャをすぐ破棄すると、使用中の資源を壊す。待ちに `poll(Wait)` を使うと Worker が止まる。

### 設計

送信ごとに `u64` の番号を 1 増やす。`on_submitted_work_done` がその番号を `AtomicU64` の完了番号へ書く。`present_in_flight` のブール値を、この番号に一般化する。コールバックは描画器を借用しない。番号だけを書く。

```rust
struct GpuTimeline {
    next: u64,
    completed: Arc<AtomicU64>,
}
```

層とプールテクスチャは、最後に読まれたか書かれた送信番号を持つ。次の占有者に渡してよいのは、完了番号がその番号以上のときだけである。`render` の先頭で `device.poll(Poll)` を 1 回呼び、完了番号を進める。`Poll` は待たない。WebGPU ではコールバックがフレームの途中で進まないので、同じ `render` 呼び出しの中で送った層は、その呼び出しの中では再利用しない。

再利用できる層が無いときは、合成を `Blocked` にするか、今の `present_deferred` と同じようにフレームを返す。`poll(Wait)` は呼ばない。50 ms の `presentSettled` も変えない。

破棄するオブジェクト（ストロークの終了、プールからの追い出し、作り替えたカバレッジ）は、番号を付けて `Retired` に置き、完了番号が追いついてから `destroy()` する。`Drop` では解放しない。追いつくまでのあいだ、その実体はピークに残る。1 フレーム分の遅れで、Worker は止まらない。上書きする層には `destroy()` を呼ばない。

アトラスへの `write_texture`（1115 行）も同じ番号で止める。合成中にピンした層は、今の `pinned` に加え、番号が完了するまで追い出し対象にしない。

### 触るファイル

- `crates/alto-gpu/src/renderer.rs`：`Retired`、`submit_composite`、`render` の poll と present、`upload_timed`、`cache_slot`。
- `crates/alto-gpu/src/renderer/m5.rs`：プールへ返すときと、追い出すときの番号。
- `crates/alto-gpu/src/renderer/vector.rs`：カバレッジを捨てるとき（558–562 行）の番号。

`submittedWorkDone`（`alto-wasm/src/lib.rs` 1127–1137 行）と Worker の 50 ms 制限は、別の変更が触っている。ここからは呼ばない。タイムラインは描画器の中の `on_submitted_work_done` だけを使う。

### 削減の見込み

バイト数は減らない。使用中のテクスチャを壊さずに、小さい予算とプールの再利用をできるようにする前提である。

### リスク

- 完了を待たずに上書きすると、前の present の画素が次のタイルに変わる。番号の比較を、書き込みの前に置く。
- 完了待ちを `Wait` にすると、フレーム予算の 8 ms を GPU 待ちが食う。`Poll` と、再利用できなければ遅延、に限る。
- 破棄を完了まで遅らせると、ピークが 1 フレーム分高くなる。`effects` と `stroke` の計上に、`Retired` でまだ `destroy` していない実体を含める。含めないと、予算の下で実体が残る。

### テスト

- 番号が完了していない層へ書き込まないこと。完了した層は書き込めること。全部が使用中なら、待ちに入らず `deferred` か `Blocked` を返すこと。
- 前の present が飛行中に、その present が参照している表示層の画素が変わらないこと。読み戻しで見る。
- プールのテクスチャを、完了前に `destroy` していないこと。デバイス喪失のあとは、今どおり資源をまとめて捨てて作り直す。

## 実装の順序

1. **計測（1 節の型と API）。** 挙動は変えない。合計が今の `gpu_bytes` と一致することを先に固定する。B6 の表をこの形で取り、以降の差を区分で見る。
2. **ストロークをキャンバスとの交差で切る。** 画素は変えない。この B6 では約 190 MiB 減り、ピークは約 294 MiB になる。484.8 を実際に下げるのはここである。
3. **送信番号（5 節）。** 退避とプールが上書きを始める前に入れる。
4. **予算に合わせた層数（4 節）。** 512 MiB では固定は 193 MiB のまま。128 MiB と 256 MiB の超過と過大予約をやめる。
5. **退避の箱と最粗のピン（3 節）。** 層数が予算で頭を持つようになってから、どれを残すかが効く。
6. **M5 のプール（2 節）。** 番号と、新しい画素テストが揃ってから。B6 の数字は動かない。

各段階で、Rust のテスト、clippy、SwiftShader の機能テストを通す。Metal の B6 は区分付きで記録し、CI の合否には使わない。

## やらないこと

- **中間テクスチャを `Rgba16Float` から 8 ビットへ下げることはしない。** ぼかしと合成の画素が変わる。描画バージョンを上げ、ハッシュの回帰テストを付ける必要がある。描画バージョンは今は無く、この設計では足さない。
- MSAA の段数、グリフアトラスの解像度、ピラミッドの平均の式を変えること。同じ理由で画素が変わる。
- CI を Metal の絶対値（484.8 MiB など）で落とすこと。CI は SwiftShader で、合否は計上値の一致と、main に対する画素と機能の相対比較にする。
- `clear_texture`。wgpu の WebGPU バックエンドには無い。空のレンダーパスの `LoadOp::Clear` を使う。
- 退避タイルの CPU 側圧縮保持。最初の実装には入れない。WASM の 1024 MiB 枠を使うためである。
- スパーステクスチャでアトラスの空層を物理的に外すこと。このコードが使う WebGPU の範囲では、層ごとのレジデンシーは使えない。空層は計上の `fixed - live_fixed` で見えるようにする。
- ベクターのカバレッジを M5 のプールへ載せること。計上は `vector` に入れる。プールの共有は、M5 が安定してからの候補にする。
- 進行中の別変更が触る、GPU と CPU の画素一致テストの既存本体、`presentSettled` の 50 ms、README、LICENSE。ぼかしの再利用テストは新しい関数として隣に足す。
- `Drop` に解放を任せること。WebGPU では解放されない。`destroy()` を、送信番号の完了のあとで明示的に呼ぶ。
