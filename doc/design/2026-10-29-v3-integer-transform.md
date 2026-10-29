# v3 整数変換

作成日：2026-10-29（Asia/Tokyo）

状態：設計のみ。この文書は実装しない。実装は次の 1 本の PR で入れる（下の「実装」）。描画バージョン 1 と 2 の変換の計算（`alto_core::Affine2::concat` / `inverse`、`sample::integer_inverse_v2`、JS の `multiply` / `inverseF64` / `quantizeQ16V2`）は凍結したままである。

関連：[v3 整数合成](2026-10-28-v3-integer-compositing.md)、[v3 整数マスク](2026-10-28-v3-integer-mask.md)、[正確な CPU 参照](2026-10-27-exact-cpu-references.md)。出典は PR [#38](https://github.com/NobukiAkiyama/alto-engine/pull/38) の Codex レビュー P1-1（[コメント](https://github.com/NobukiAkiyama/alto-engine/pull/38#issuecomment-6477123007)）。

## 前提

#38 で、シェーダが読む値（Q16 の逆行列、Q8 の画素中心、タイル選別、キャッシュの無効化）は整数になった。しかし、その Q16 の逆行列を作るホスト側はまだ f64 である。

1. 親から子への変換の合成。`multiply`（`crates/alto-gpu/src/renderer.rs`）は `Affine2::concat` の f64 の積和である。
2. 逆行列。`inverse` は `Affine2::inverse`（`crates/alto-core/src/lib.rs`）で、f64 の行列式と除算を使う。特異かどうかは `det.abs() < 1e-12` で決まる。
3. Q16 化。`sample::integer_inverse` は f64 の逆行列を受け取り、`v * 65536` を丸める。この丸め自体は正確である（`f64::round`。Codex P2-1 は #38 で修正済み）。しかし入力の逆行列が、もう f64 の丸めを何段も通っている。

呼び出し場所は次のとおり。

| 場所 | 計算 |
| --- | --- |
| `renderer.rs` の `PlanTask::Node`（plain） | `multiply(world, n.transform)`、`inverse(world)`、`plain_draw_v3` → `integer_inverse` |
| `renderer/m5.rs`（モード 18、マスク、ベクターのカバレッジ） | `multiply(parent, node.transform)`、`multiply(world, mask.transform)`、`mask_sampler` → `integer_inverse(inverse(transform)?)` |
| `alto-scene/src/damage.rs` | `plain_scene` が `Affine2(world).inverse().is_some()` で描くかどうかを決める |
| `cpu_composite.rs` の `walk` | `Affine2::concat` と `Affine2::inverse`、そのあと共有の Q16 化 |
| `tests/e2e/cpu-composite.js` | `walk`、`multiply`、`inverseF64`、`quantizeInverse` |

f64 の積和は、式と評価順が同じなら IEEE-754 で一つの値に決まる。しかし、その一致は Rust と JS の式を手で揃え続けることに頼っており、仕様として書かれていない。JSON から読んだ f64 が Rust と JS で同じビットになる保証もない（決定 5）。そのうえで、行列式が `1e-12` の近くにあると描くか描かないかが変わり、Q16 の結びの近くでは f64 の 1 ulp の違いが別のテクセルを読ませる。v3 の約束は「描画経路に float なし」なので、この部分も整数で定義する。

## 範囲

| 対象 | 対象外 |
| --- | --- |
| v3 の描画で、文書の変換からシェーダの Q16 逆行列までのすべて（合成、逆行列、量子化、特異の判定） | 表示（`present-q14.wgsl` のビュー変換、sin/cos、bilinear）。保存されるタイルに書き戻さない |
| `transform` と `mask_transform` の両方 | 編集。ブラシの入力の写像（`alto-wasm` の `input_inverse`）、選択からマスクへの写像（`alto-engine/src/m5/mod.rs`）、平坦化（`edit.rs`）。結果はバイトとして文書に入る |
| Rust の GPU レンダラー、Rust の CPU 参照、JS の CPU 参照 | ベクター塗り、ぼかし、テキストの v3 実装。ただし、それらの変換の入力はこの文書の `FixedAffine` にする |
| `plain_scene` の v3 での特異の判定 | damage の f64 の矩形（過剰な無効化はフォローアップ。正しさには影響しない） |

## 決定

### 1. 保存形式：文書は f64 のまま、描画の入口で整数にする

文書の `Affine2` は f64 の 6 要素のまま変えない。v3 の描画は、各ノードの局所変換を入口で一度だけ整数の `FixedAffine` に量子化し、そのあとは整数だけを使う。

整数の行列を文書に保存する案は採らない。理由は三つある。

- 文書の形式、SDK の `Affine` 型、編集（`edit.rs` の concat や平坦化）を v3 だけ別の型にすることになる。v1/v2 の文書と共有しているコードが二重になる。
- ピアが受け取るのは f64 のビット列であり、それ自体がすでに端末をまたいで同じ値である。描画が「ビット列から決まる整数の関数」であれば、決定性はそれで足りる。マスクの文書が「生成は編集、描画はバイトから」と分けたのと同じ線引きである。
- 量子化を入口で一度だけ行えば、保存した値がそのまま正規形として扱われ、不可逆な変換が文書に残らない。

条件として、JSON の f64 が Rust と JS で同じビットに読まれることが要る。下の「決定 5」で扱う。

### 2. 型と量子化

```text
FixedAffine {
  m: [i64; 4]   // a, b, c, d（Affine2 と同じ並び、列優先）。Q32。|m| < 2^46（|値| < 2^14）
  t: [i64; 2]   // tx, ty。Q16。|t| < 2^47（|値| < 2^31 px）
}
```

f64 から量子化する規則：`v * 2^32`（並進は `v * 2^16`）を 0 から遠い側へ丸める。2 のべき乗を掛けても値は丸まらず（オーバーフローは範囲の検査で弾く）、`f64::round` は値そのもので結びを決めるので、この量子化は f64 の上で正確に計算できる。JS では既存の `roundHalfAway` を使う（`Math.round` は使わない）。NaN、無限大、範囲外の値は `None` を返し、v3 はそのノードを `GpuUnsupported` で拒否する。今の「transform has no Q16 inverse」と同じ扱いで、値をクランプすることはない。

線形部分を Q32 にする理由は、逆行列の精度にある。Q16 のまま合成すると、スケール 0.01 は 655/65536（相対誤差 5.5e-4）になり、逆行列が 100 ではなく 100.05 になる。原点から 1 万 px 離れたところで 5 px ずれる。Q32 なら相対誤差は 2^-33 以下で、v2 の f64 の逆行列を Q16 にしたときとの差は、結びの近くにある値だけに限られる。

範囲の上限は、下の i128 の計算がオーバーフローしないように選んでいる。スケールは 16384 倍まで、並進は ±2^31 px までになる。シェーダの Q16 の逆係数はもともと `i32`（スケール 2^-15 以上）なので、実際に効くのは逆行列の側の制限である。

### 3. 合成：i128 で計算し、段ごとに 0 から遠い側へ丸める

親 `P` と子 `L` の合成 `W = P · L` は、`Affine2::concat` と同じ式を整数で計算する。

```text
W.a  = rq32(P.a·L.a + P.c·L.b)        W.c  = rq32(P.a·L.c + P.c·L.d)
W.b  = rq32(P.b·L.a + P.d·L.b)        W.d  = rq32(P.b·L.c + P.d·L.d)
W.tx = rq32(P.a·L.tx + P.c·L.ty) + P.tx
W.ty = rq32(P.b·L.tx + P.d·L.ty) + P.ty
```

`rq32(n)` は i128 の `n / 2^32` を 0 から遠い側へ丸める関数である。積は 2^92 未満、和は 2^93 未満なので、i128 に収まる。結果が上の範囲を出たら `None` を返す。合成の順番は、根から葉へ向かって左から畳み込む（今の `walk` と `PlanTask::Node` の順番と同じ）。丸めがあるので結合則は成り立たないが、順番が決まっているので値は一意になる。根は単位行列 `m = [2^32, 0, 0, 2^32]`、`t = [0, 0]` である。

木の深さは `MAX_TREE_DEPTH = 128` までなので、丸めの累積は 128 × 2^-33 に収まる。

### 4. 逆行列：有理数は保存せず、一つの除算で一度だけ丸める

逆行列を有理数として持ち回ることはしない。整数の世界行列から、シェーダが読む Q16 の `IntegerInverse` を直接計算する。係数ごとに有理数の割り算を正確に一度だけ行い、丸めもそこで一度だけ行う。

```text
Det = A·D − B·C                       // Q64、i128 で正確。|Det| < 2^93
Det == 0 なら特異。そのノードは描かない（v2 の 1e-12 の閾値を置き換える）

inv.m[0] = rdiv( D · 2^48, Det)       inv.m[2] = rdiv(−C · 2^48, Det)
inv.m[1] = rdiv(−B · 2^48, Det)       inv.m[3] = rdiv( A · 2^48, Det)
inv.tx   = rdiv((C·TY − D·TX) · 2^32, Det)
inv.ty   = rdiv((B·TX − A·TY) · 2^32, Det)
```

`A..D` は Q32、`TX, TY` は Q16 である。`rdiv(n, d)` は、`n / d` を 0 から遠い側へ丸める正確な整数の除算である。符号を外し、`(2|n| + |d|) / (2|d|)` を u128 で計算してから符号を戻す。分子は 2^126 未満、`2|n| + |d|` は 2^127 未満なので、u128 に収まる。

結果の検査は今の `integer_inverse` と同じにする。係数は `i32::MIN + 1 ..= i32::MAX`、並進は `|q| < 2^53`。外れたら `None` を返し、`GpuUnsupported` で拒否する。出力は今と同じ `IntegerInverse` なので、シェーダ、`pack_wide`、`axis_unit_q16`、`v3_reach` は変わらない。単位行列の世界行列では、逆行列もちょうど `65536` と `0` になる。

`plain_scene`（`damage.rs`）の特異の判定は、v3 の場面では `Det == 0` に置き換える。そうしないと、`0 < |det| < 1e-12` の層を plain の側だけ外すことになる。

### 5. JSON の f64 を Rust と JS で同じビットに読む

`serde_json` は既定では「ほぼ正確」な浮動小数点のパーサーで、正確に丸めるには `float_roundtrip` 機能が要る。JS の `JSON.parse` は正確に丸める。既定のままでは、まれな入力で Rust と JS の f64 が 1 ulp ずれ、そのずれがそのまま Q32 の結びに届く。

実装 PR で、ワークスペースの `serde_json` に `float_roundtrip` を有効にする。これで v1/v2 の文書も正しく丸めた値で読まれるようになるので、v1/v2 のハッシュ（`tests/parity/render-v1.txt`、`cpu-reference.txt`、`SHA256SUMS`）が一つも動かないことを、同じ PR の CI で確かめる。動いた場合は機能を有効にせず、v3 の変換だけ文字列から読み直す方法を別途考える。そのときはこの文書を改訂する。

### 6. 回転の sin/cos：描画では評価しない

描画は sin/cos を計算しない。文書には行列が入っている。sin/cos は、SDK の `setTransform`（`packages/alto-sdk/src/transaction.ts`）が編集のときに `Math.cos` / `Math.sin` で一度だけ計算し、その結果のビットが文書に入る。

`Math.sin` は JS エンジンごとに最後の 1 ulp が違うことがある。そのため、二つの端末で同じ角度を入力しても、保存される行列は違いうる。しかしこれは編集の違いであり、ブラシのストロークと同じ扱いになる。同じ文書のビットからは同じ画素になる。

Q32 の量子化には、90° の倍数を正確にする副作用がある。`Math.cos(Math.PI / 2) = 6.1e-17` は `6.1e-17 · 2^32 = 2.6e-7` なので 0 に丸まる。90° 回転の世界行列は、係数がちょうど 0 と ±2^32 になり、`axis_unit_q16` で nearest になる。SDK を整数の CORDIC などで決定的にする案は、描画の約束に必要ないので採らない。

## 実装

1 本の PR で入れる。

1. `crates/alto-core/src/lib.rs` の `Affine2` の隣に `FixedAffine`、`Affine2::fixed() -> Option<FixedAffine>`、`FixedAffine::concat`、`FixedAffine::det`、`rq32`、`rdiv` を置く。`alto-scene` の `plain_scene` も使うので、`alto-gpu` ではなく `alto-core` に置く。`crates/alto-gpu/src/sample.rs` には `fixed_inverse(&FixedAffine) -> Option<Option<IntegerInverse>>` を置く（外側の `None` は範囲外、内側の `None` は特異）。v2 の `integer_inverse_v2` と `pack_inverse` には手を入れない。
2. `renderer.rs`：`PlanTask::Node` の `world` を、v3 のときは `FixedAffine` で持つ。`plain_draw_v3` は f64 の逆行列ではなく `IntegerInverse` を受け取る。v1/v2 の分岐は `multiply` / `inverse` のまま残す。
3. `renderer/m5.rs`：`stack` の `parent`、`mask_sampler`、ベクターのカバレッジ用の `multiply(world, mask.transform)` / `path.transform` を、v3 では `FixedAffine` にする。`RENDER_VERSION_EXACT` での分岐は、`blend` / `stage` / `raw_node` と同じ形にする。
4. `alto-scene/src/damage.rs`：v3 の `plain_scene` の特異の判定を `Det == 0` にする。
5. `cpu_composite.rs`：v3 の `walk` を `FixedAffine` で行い、`Op::Draw` に `IntegerInverse` を載せる。
6. `tests/e2e/cpu-composite.js`：`fixedAffine`、`fixedConcat`、`fixedInverse` を BigInt で書く。f64 から BigInt への変換は、量子化して整数になった f64 からだけ行う（`BigInt(x)` は整数の f64 なら正確である）。v3 の `walk` / `lookupV3` / `renderPlainV3` はこれを使う。
7. ワークスペースの `serde_json` に `float_roundtrip` を有効にする（決定 5）。
8. v3 のハッシュはこの PR で動いてよい。v3 はまだ固定していない（固定はリリース作業の 4 番）。v3 の期待値は同じ PR で更新し、どのケースが動いたかを PR の説明に書く。

### float の静的検査

#38 のシェーダ関数に対する float の禁止検査を、ホストにも広げる。v3 の描画で到達する Rust の関数（`FixedAffine` の各関数、`fixed_inverse`、`plain_draw_v3`、`mask_sampler` の v3 の分岐、`v3_reach`）の本体に `f32` / `f64` が出てこないことを、テストで確かめる。例外は `Affine2::fixed` の入口（f64 を読む唯一の場所）と、`f32::from_bits` での整数の運搬だけで、例外は名前で列挙する。

## テスト

| テスト | 内容 |
| --- | --- |
| 量子化の結び | `(k + 1/2) · 2^-32` と、その上下 1 ulp を正負それぞれで。並進は `2^-16` で同じく。`-0.0`、NaN、±∞、範囲の境界（`2^14` ちょうど、その 1 ulp 下）、subnormal |
| 合成 | 深さ 128 のスケール 0.5 と 2 の交互、30° の回転を 12 回重ねたもの、範囲を出る合成（`None`）。Rust の結果を i128 の素朴な実装と照合する |
| 逆行列 | 単位行列、90°/180°/270°（`Math.cos` / `Math.sin` で作ったもの）、`[1, 2, 2, 4, ·, ·]`（`Det == 0`）、`Det = ±1`（最小の非零）、`rdiv` の結びになる分子、逆係数が `i32` を出るもの |
| Codex P2-1 の再現 | `[1, 0, -(2^-17 - 2^-70), 1, 0, 0]` で、先頭の画素が `[0, 0, 0, 255]` になること。実レンダラーの回帰テストにする |
| Rust と JS の一致 | 固定シードの乱数で 10,000 個の局所変換の鎖（深さ 1〜8）を作り、`IntegerInverse` が Rust と JS で全部一致すること。乱数の列は Rust から書き出し、JS がそれを読む |
| 描画の一致 | 既存の v3 parity のケース（plain、M5 の各モード、マスク）に回転とスケールのケースを足し、GPU と Rust の CPU 参照と JS の CPU 参照が全バイト一致すること |
| v1/v2 不変 | `render-v1.txt`、`cpu-reference.txt`、`SHA256SUMS` の v1/v2 の値に差分がないこと。`PRESENT_DEFER_LIMIT = 40` も不変 |

## 受け入れ条件

- v3 の描画経路で、文書の f64 のビットからシェーダの入力までが整数の関数として定義され、Rust の GPU、Rust の CPU 参照、JS の CPU 参照で `IntegerInverse` がビット一致すること。
- v3 の描画で到達するホストの関数に、上の例外以外の float がないこと（静的検査）。
- v1/v2 のハッシュが変わらないこと。

## 未決

- 決定 5 の `float_roundtrip` で v1/v2 のハッシュが動いた場合の代案。
- 範囲（スケール 2^14 倍、並進 2^31 px）が実際の文書で足りるか。足りないときは、`rdiv` を 256 ビットにして範囲を広げる（JS は BigInt なので変更不要）。
