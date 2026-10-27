# ベクター描画の改善設計

作成日：2026-10-26。状態：**比較基盤、P1、P2a を実装・検証済み。** P2b / P3 は今回は採用しない。以下の設計時の見積もりと目標値に対する実測・検証範囲は、[実装結果](../verification/VECTOR-OPTIMIZATION-RESULTS-2026-10-26.md)を参照。

後続の密集ベクター改善は [順序を保つ MRT8 の設計](2026-10-26-dense-vector-mrt.md)へ分けた。本書と以下の先行測定は disjoint batching 段階の記録として保持する。

対象：`35be75a` と現在の作業ツリー。既存の未コミット変更を含む。GPU 喪失の調査は別の問題として継続中であり、本設計がその障害を解消するとは扱わない。

## 1. 採用する方針

1. 現行版を計測し直して、描画パス・中間画像・一時 GPU 資源の費用を分ける。
2. 現行の lyon、4x MSAA、R8 カバレッジ、RGBA16F の合成を保ったまま、資源の再利用と安全なバッチ化を行う。
3. 改善後も遅い用途に対して、vello 0.11.0 を実験用バックエンドとして比較する。画質・精度・統合後の費用を通過条件にする。
4. 測定した利益が小さければ、その段階で止める。独自の compute 型ベクター描画器を新たに作る計画にはしない。

文書、保存形式、Undo/Redo、版管理、SDK の安定 API は本設計の変更対象ではない。テキストの字形アトラスも初期段階では既存経路を使う。

## 2. 設計時に確認した現状

| 箇所 | 現在の処理 | 設計への影響 |
|---|---|---|
| `crates/alto-vector/src/lib.rs` | lyon が塗り・線を別の Mesh に準備する | 同じパスの塗りと線の順序を保存する |
| `crates/alto-gpu/src/renderer/vector.rs` | Mesh のバッチごとにカバレッジ描画と色合成を分ける | render pass の数がバッチ数に比例する |
| 同上 | 各バッチで uniform buffer と bind group を作る | メッシュがキャッシュ済みでも作成費用が残る |
| 同上 | ROI 全体サイズの MSAA / resolve テクスチャを再利用する | テクスチャを毎回新規作成している、とは説明しない |
| `crates/alto-gpu/src/renderer/m5.rs::vector` | RGBA16F の出力へ描き、マスク・効果・合成へ渡す | RGBA8 の外部描画器への交換には精度の検証が必要 |
| `crates/alto-wasm/src/vector_probe.rs` | 512×512、100 パス、塗り＋線、両方式の準備済みデータを反復描画 | 文書編集全体の比較ではない |
| `alto-vector::VectorRasterizer` | 関連型を持つ準備処理の trait | vello の GPU 提出まで既に抽象化されている、と解釈しない |

100 個の可視パスに塗り・線がある場合、現行構造は概ね `100 × 2 × 2 + 出力の clear`、約 401 render pass になる。これはコードからの見積もりであり、現行 WASM の実測カウンターではない。queue.submit が 401 回という意味でもない。

10月18日の保存済み比較は、lyon＋Alto の warm 中央値 25.9 ms、vello 0.11.0 は 0.8 ms。AA は前者が 4x MSAA、後者が Area。現行版の再測定でも、同じ画素結果の比較でもない。倍率をそのまま今回の期待値にしない。

参照：[`doc/verification/VECTOR-COMPARISON.md`](../verification/VECTOR-COMPARISON.md)。

## 3. 計測を先に揃える（P0）

### 3.1 比較コードの条件

- 同一コミットから LegacyLyon / BatchedLyon / VelloExperimental を作る。vello を含むビルドは別の比較用 WASM とする。
- 全方式が同じ adapter / device / queue を使う。Chrome の版、GPU、DPR、ROI、形式、AA、feature、WASM SHA-256 を JSON に残す。
- 主性能判定は実 GPU で行い、SwiftShader は主に正しさと障害経路の検査に用いる。
- renderer 初期化、シーン準備、初回描画、編集後、warm 反復を別々に測る。warm の一部分だけを高速化率にしない。
- CPU のエンコード時間、submit まで、GPU queue 完了までを分ける。対応環境だけ GPU timestamp を追加し、未対応時に GPU 時間と偽って表示しない。
- 3 回のウォームアップと 20 フレームの旧条件も再現するが、採用判定では独立プロファイル 5 回以上、warm 100 フレーム以上を収集する。方式の実行順は交互にし、外れ値と失敗を残す。
- 正しさのための readback は計時の外。GPU 完了待ちは各方式で同じ境界に置く。
- `vector_probe.rs` は、本体と同等の資源解放を行うようにする。現行の本体は `resources.retired` を回収するが、単体 probe の反復には同じ処理が見当たらない。これは現在の比較コードの確認事項であり、10月18日の速度差の原因と断定するものではない。
- 比較用 readback buffer も unmap 後に解放する。ブラウザの GC が回収するまで放置する違いを比較に混ぜない。

### 3.2 追加する内部カウンター

`vectorRenderPasses`、`coveragePasses`、`paintPasses`、`geometryDraws`、`paintDraws`、`uniformBuffersCreated`、`bindGroupsCreated`、`bufferUploadBytes`、`coverageClearPixels`、`resolvePixels`、`queueSubmits`、`meshHits/Misses`、`candidatePaths`、`groups`、`overlapFallbacks`、`groupingCpuMs`。

clear / resolve の画素数は論理的な処理範囲の集計であり、実際の DRAM 転送量ではない。GPU の最適化や圧縮を含む物理帯域を推定した数値として出さない。

GPU メモリは管理している確保量、vello の推定値、ブラウザが報告するメモリを区別する。vello 内部の全確保量を取れない段階では「総 GPU 使用量が予算内」とは保証しない。

## 4. バッチ化の前に資源を再利用する（P1）

描画順序と pass 構成を変えず、まず一時資源作成の費用だけを減らす。

- `VectorUniformArena` に必要な Uniform をまとめて用意する。
- `minUniformBufferOffsetAlignment` に合わせ、動的 offset を使う geometry bind group を共有する。上限は device の実際の limits で計算する。
- `write_buffer` で未使用の領域へ一括アップロードする。mappedAtCreation を使うバッファを図形ごとに作らない。
- coverage の view と paint 用の bind group は、テクスチャの世代が変わったときに作り直す。毎バッチで同じ view を作らない。
- メッシュの vertex / index buffer は既存キャッシュを使う。初期段階では巨大な結合メッシュや複雑な GPU allocator を追加しない。

### 同じ uniform 領域を使い回さない

一つのフレーム内で、`write_buffer(offset=0)` と「後で submit する描画」を繰り返すと、複数描画が最後の値を見る可能性がある。**エンコード済みで未提出の全描画に対し、一意の offset を割り当てる。** M5 が同じ encoder で複数の ROI を処理する場合も含む。

arena は同一 queue 上の提出境界を把握して管理する。再利用する領域への次の write が前回の submit より後になることを保証する。submit 前の領域を上書きしない。容量変更した旧 buffer は、その buffer を使う submit の後で既存の `Retired` によって解放する。

arena に収まらなければ、上限付きでページを追加するか、当該バッチを既存の一時 buffer 経路へ戻す。別 submit を暗黙に増やしたり、毎フレーム GPU 完了待ちを入れたりしない。確保量と peak を計測する。

P1 は legacy と同じ画素結果を必須とする。ここで描画時間が改善すれば、P2 と別に改善幅を記録する。

## 5. 重ならないパスをまとめる（P2）

### 5.1 中間画像の座標と精度を保つ

初期案は mask atlas への詰め直しではなく、**既存と同じ ROI サイズ・座標変換の R8 coverage 画像を共有する方式**とする。

atlas の座標へ移すと、頂点変換の浮動小数点誤差や AA 境界が変わり得る。初期段階では現在の shader の計算順と MSAA の設定を保ち、パス数・clear・resolve の削減効果を先に測る。

### 5.2 安全にまとめられる条件

対象は、一つの vector content を描く既存の `render` 呼び出しの内側に限る。別レイヤー、グループ境界、効果境界を越えてまとめない。

各 StyledPath の全 Mesh バッチについて、既存コードと同じ変換後の bounds を計算する。塗り・線・join・cap を含めた Mesh の bounds を用い、AA と端数に備えて保守的に拡張して ROI に切り詰める。

同じグループに入れるパスは、その**全バッチの bounds の和集合を覆う矩形が互いに交差しない**ことを条件にする。接触や数値的に曖昧な場合も交差として扱う。形状が本当は重ならなくても bounds が重なる場合は、無理にまとめない。

元のパス順を走査して連続グループを作る。新しいパスが現在のグループと交差するときはグループを確定し、次のグループを始める。

小さい入力は単純走査、大きい入力は画面空間の区画索引を使い、最終判定は厳密な矩形交差にする。区画の占有数、候補比較数、グループサイズに上限を設ける。提案初期値は最大 128 パス / グループ、4,096 比較 / グループ。上限に達したらグループを閉じる。これらはベンチマークで調整する値で、最適値ではない。

O(n²) の判定を無制限に増やして、描画の削減分を CPU で失わない。

### 5.3 塗りと線の順序

任意のパスの塗りを全部先に描くことはしない。半透明の重なりの意味が変わるため。

上記の「パス全体が互いに重ならない」と証明したグループ内だけ、各パスのローカルなバッチ番号ごとに処理する。

```text
従来：A塗り → A線 → B塗り → B線 → …

同じグループ内の A と B の描画範囲が完全に離れている場合：
  coverage：A塗り、B塗り、… → resolve → 色合成
  coverage：A線、  B線、  … → resolve → 色合成
```

これにより、各パス内の塗り→線の順序は保存される。グループ間の順序も保存する。バッチ数が 1 の線だけのパスなども、ローカル番号順で扱える。

対象パス 100 個が一つのグループに入り、各パスが塗り・線の 2 バッチなら、出力 clear 1 回＋coverage / paint 各 2 回で、**約 401 → 5 render pass** が構造上の目標になる。描画速度が同じ比率で改善するという意味ではない。重なりが多い文書では、この削減は成立しない。

### 5.4 coverage と paint の発行

各グループのローカルバッチ番号について：

1. coverage 画像を一度 clear し、一つの render pass で対象 Mesh を描く。各 Mesh は既存の scissor、変換、MSAA、blend 設定を使う。
2. render pass の終了時に一度 resolve する。
3. 一つの paint pass で対象領域の色を合成する。

paint は初期実装では既存の fullscreen triangle＋scissor と動的 uniform を使い、pass だけまとめる。これだけでも correctness と pass 数の検証ができる。

その後に必要なら、ROI 内の矩形・色を instance データにして矩形の instanced draw にする。同グループの矩形が互いに交差しないため、ここに重なり順の問題はない。頂点・画素境界の一致を再検証し、差が出る場合は元の paint draw を使う。

coverage pass と、その coverage を読む paint pass は分ける。同じ texture を同じ pass の attachment と sampling 用に同時使用しない。

### 5.5 適用範囲と退避

- P2 の初期対象は通常の Paths の色描画のみ。text / glyph、ベクターマスクは既存経路を使う。
- mask / text の呼び出しを識別できる内部の用途区分を追加し、`render_text` から outline に回る部分を意図せず最適化しない。
- グループが 1 パスだけなら既存描画を使う。dense overlap では従来と同等の処理量に戻る。
- bounds が不明、数値が非有限、予算や準備上限を超えた場合は、GPU コマンドを発行する前に legacy を選ぶ。
- 提出途中の validation error や GPU_LOST を「legacy でもう一度」で隠さない。既存のエラー／GPU 復旧処理へ渡す。
- stroke の三角形を半透明色で直接重ねて描く短絡的な最適化は行わない。自己重なりで濃度が変わるため、既存の coverage union を維持する。

## 6. vello は制約を確認した実験経路にする（P3）

### 6.1 準備済み Scene と表示タイルを分けてキャッシュ

将来の実験バックエンドは内部 enum `LegacyLyon / BatchedLyon / VelloExperimental` で固定選択する。安定 SDK に自動切替 API は追加しない。

`VectorRasterizer` の準備 trait は現在のまま使用できるが、GPU の encode / submit の契約は別に設計する。準備・提出・資源寿命まで同じ trait であるかのように扱わない。

Scene の準備結果は content hash と変換の扱いを明示して再利用する。最初は node 内のパス群だけを vello に渡し、Alto のレイヤー合成や効果を重複実装しない。ROI に応じた平行移動・拡大縮小は f64 で原点を寄せてから適用する。

生成した画像のキャッシュには content hash、world transform、ROI の位置・大きさ・scale、halo、backend revision、AA、format、device epoch が必要。既存の表示キャッシュにこれらを追加する方法、または実験切替時に全表示キャッシュを無効化する方法を明示する。初期実験は後者でよい。

### 6.2 RGBA8 と RGBA16F は交換可能ではない

確認した vello 0.11.0 の `Renderer::render_to_texture` は RGBA8Unorm＋STORAGE_BINDING の出力を要求する。Alto の M5 合成途中は premultiplied RGBA16F。既存の比較でも vello の読み戻しを straight alpha から premultiplied に変換している。

実験では、出力を GPU 内の変換パスで Alto 側へ渡す。

```text
同じ device / queue
  vello → RGBA8Unorm の一時 texture
        → alpha と色空間を確認した変換パス
        → Alto の premultiplied RGBA16F target
        → 既存のマスク・効果・レイヤー合成
```

readback → CPU の色変換 → 再アップロードは挟まない。ただし RGBA8 に丸めた情報は、RGBA16F に変換しても戻らない。

同版の fine shader と実測の両方で、alpha が 0 / 微小な場合、色の符号化、出力の premultiply の扱いを確認する。sRGB の decode を根拠なく追加しない。

半透明を何層も重ねた結果、マスク境界、ぼかしの低濃度部、色の精度が受入条件を満たせなければ、vello 0.11 の本体採用を止める。単体の 0.8 ms を理由に描画品質を黙って変更しない。RGBA16F 出力のための vello の大規模 fork は、この計画には含めない。

### 6.3 提出順序を壊さない

vello の通常の `render_to_texture` は内部で GPU recording を実行する。Alto の `M5` は caller の encoder に処理をためるため、その途中から気軽に呼ぶと、未提出の先行コマンドより vello が先に提出される。

最初の実験は独立した source texture の事前生成に限る。source の必要集合を計画し、vello を同じ queue で提出した後に、変換と消費側の encoder を提出する。queue の順序で依存関係を守り、画像ごとに `gpuIdle()` を挟まない。

本体 M5 への統合を進める場合は、source の事前生成に必要な ROI / halo / mask を列挙する計画フェーズを作る。それが難しい経路は legacy のままとし、暗黙の submit を追加して対処しない。低レベル API や fork による同一 encoder 統合は、別途費用を評価してから検討する。

初期は一度に一つの ROI を処理する scratch を使い、ROI の数だけ vello の renderer / texture を保持しない。通常の surface / Canvas の構成や GPU 喪失対策は変更しない。

### 6.4 採用の前提

- renderer 初期化時の機能と limits、validation error を確認し、未対応時は描画前に legacy を選択する。
- vello 内部資源を含むメモリの計測方法と解放方法を確認する。外部 texture の予算だけで全体を管理したことにしない。
- GPU_LOST は同じ device での別 backend 実行では復旧しない。device epoch を進め、backend の資源も再構築する。
- 比較で使う vello は `=0.11.0` を基準とし、新しい vello_gpu / CPU 版の説明や性能値を混ぜない。
- vello を採用したビルドでは、WASM サイズ、圧縮後サイズ、初期化、初回表示も採用条件に入れる。

## 7. 画素の正しさと回帰検証

### P1 / P2：従来画像と同じ結果を必須とする

同じ adapter 上で旧経路と新経路を強制選択し、同じ RGBA16F target および同じ表示／export 経路で比較する。RGBA8 に変換した後だけ比較して、内部の精度差を隠さない。

必要な代表ケース：

- 離れたパス、bounds だけ接触するパス、1 px の隙間、半透明の重なり。
- 塗り＋線、線だけ、自己交差する線、miter / round / bevel、cap、穴、even-odd / non-zero。
- 負座標、大きな座標、回転、非等方拡大、shear、ROI の外側から横切る形、タイル境界、極端な倍率。
- 少数パス、100 / 1,000 パス、ほとんど全パスが重なる入力、準備の上限を超える入力。
- 1 回の encoder に複数の vector 呼び出しを入れたケース。uniform の上書き・取り違えを検出する。
- mask / effect / text が legacy のまま正しく合成されること。後で適用範囲を広げるときは専用の画素検査を追加する。
- 編集、変形、Undo/Redo、保存再読込、GPU 復旧後のキャッシュ再作成。

失敗時には seed、元パス、変換、ROI、backend、画像差分とカウンターを保存する。AA を緩めて P2 の差分を見逃すことはしない。

### P3：AA 差と精度差を独立に判断する

vello と legacy の AA は異なる。境界の差を無条件のバグともしないし、境界を全部無視して合格にもしない。解析的に評価できる輪郭、参照用の高解像度画像、透明色・重なり・mask / effect の品質を別々に判定する。品質条件は測定前に固定する。

## 8. 性能の受入条件

以下は今後の**目標値**であり、達成済みの数値ではない。

| 対象 | 初期の採用条件 |
|---|---|
| P1 の warm 反復 | per-batch の buffer / bind group 作成を O(パス数) から定常的な再利用へ削減。画素一致、処理時間の悪化なし |
| P2 の離れた 100 パス | 条件成立時に約 401 → 5 pass をカウンターで確認。GPU 完了込み中央値は現行再測定値の 50% 以下を目指す |
| 重なりが多い 1,000 パス | グループ化が上限内で終了。複数回の中央値で 10% 超の悪化が続くなら条件を狭めるか P2 を選択しない |
| ラスターのみ / 小さい text | 経路の変更なし。B1〜B8 の比較で継続的な悪化がないこと |
| メモリ | P2 は coverage texture の枚数・面積を増やさない。arena の peak は既存のメモリ予算に含める。反復後に一時資源が増え続けないこと |
| vello の本体統合 | 単体だけでなく、ROI 生成・変換・effect・合成・初回準備を含めた実文書で利益が残ること。画質と全体予算を満たすこと |
| 安定性 | 意図的な故障注入とは別に、予期しない validation / GPU loss があればその実行は失敗 |

絶対時間の目標は P0 の現行結果で決める。SwiftShader の速さで実 GPU の採用判断をしない。外部の Canvas 2D 版などとの優劣は、このベクター改善だけでは証明できない。

## 9. 実装する順序と変更箇所

| 段階 | 作業 | 主な変更候補 | 完了条件 |
|---|---|---|---|
| P0 | 計測・比較コードの整合、資源の解放、現行結果の保存 | `vector_probe.rs`、比較用 example、`vector.rs` の内部統計 | 現行の pass 数・時間・確保量を説明できる |
| P1 | uniform arena と bind group / view の再利用 | `renderer/vector.rs`、必要な geometry shader / pipeline layout | 同じ画素結果、一時資源作成を削減、反復で増加しない |
| P2a | bounds 計画器とグループ発行。paint draw は既存方式 | 新規 `renderer/vector/batch.rs`、`renderer/vector.rs`、用途を渡す `m5.rs` | pass 削減と全画素一致、重なり時の退避 |
| P2b | 必要な場合だけ paint の instancing | `vector-paint.wgsl`、`vector.rs` | P2a より実測で有利、境界も一致 |
| P3a | vello の RGBA8 精度・alpha・ROI 変換を単体で検証 | 比較専用 adapter / shader | 高速性と品質の両方を満たす見込みを判断 |
| P3b | 必要集合の事前生成を通した M5 統合の実験 | 内部 source 計画、optional dependency、メモリ計測 | 統合後の利益、順序、予算、復旧が成立 |

P2 が必要性能を満たせば、P3 は採用しなくてよい。各段階を単独で切り戻せる形にし、既存の変更をまとめて巻き戻さない。

コード変更時の必須確認は Rust / WASM の検査、ブラウザの機能・M5〜M8・SDK 回帰、実 GPU の性能比較。WASM を変更した段階では aarch64-apple-darwin で `sh scripts/update-wasm-sums.sh` を実行し、`wasm/SHA256SUMS` と build metadata を更新する。バイナリはコミットしない。

## 10. 根拠となる仕様・ソース

- [vello 0.11.0 の設計説明](https://docs.rs/vello/0.11.0/vello/)：compute 中心の処理方式。GPU による並列化を説明している。
- [同版 Renderer API](https://docs.rs/vello/0.11.0/vello/struct.Renderer.html#method.render_to_texture)：描画・出力 texture の契約。インストール済み `vello-0.11.0/src/lib.rs` の同メソッドも確認した。
- [同版の公開ソース](https://docs.rs/vello/0.11.0/src/vello/lib.rs.html)：出力形式と recording の実行を確認する参照先。
- [WebGPU の render pass](https://gpuweb.github.io/gpuweb/#render-pass-encoder)：attachment の clear / load と resource usage の契約。scissor を小さくしても attachment の LoadOp::Clear がその矩形だけになると仮定しない。
- [WebGPU queue の完了待ち](https://gpuweb.github.io/gpuweb/#dom-gpuqueue-onsubmittedworkdone)：提出済みの queue 処理を待つ。compositor の表示完了や GPU 喪失の解消を保証する API と解釈しない。
- [`doc/verification/VECTOR-COMPARISON.md`](../verification/VECTOR-COMPARISON.md)：古い単体比較の範囲と画素差。
- [`doc/verification/GPU-LOSS-SURFACE-RECORDING-2026-10-25.md`](../verification/GPU-LOSS-SURFACE-RECORDING-2026-10-25.md)：独立して残る GPU 喪失の調査。

**実装では lyon / MSAA / RGBA16F を保った P1・P2a を採用した。vello の本体統合や paint の instancing は、今回の目標を満たすために追加しない。** 完了した検査と測っていない範囲は実装結果に分けて記録する。
