# 初回表示の継続性と readback 予算管理の設計

作成日：2026-10-27（Asia/Tokyo）

状態：設計時点の基準と受入条件を記した文書。2026-10-27 に実装を追加した。実装済みの範囲・実行した検証・未検証の故障条件は [実装と検証記録](../verification/PRESENTATION-READBACK-RESULTS-2026-10-27.md) を参照する。下記の受入表全件を実測済みという意味ではない。

基準実装：PR #6 の head adf3c342e743d105c7ee1fb89082053f41a18bc1。main への統合は 1f16c8094bdb847ddd18fa8ffdf7ab24621b80f4。手元の実装ファイルを読んで設計した。行番号は変化するため、参照はファイルと関数名を基準とする。

関連：[GPU メモリ設計](2026-10-26-gpu-memory.md)、[その実装・検証記録](../verification/GPU-MEMORY-RESULTS-2026-10-26.md)、[コールド表示設計](COLD-RENDERING-DESIGN.md)、[再開可能な低倍率先読み設計](RESUMABLE-LOW-ZOOM-PREFETCH-DESIGN.md)。

## 1. 採用する方針

表示は「公開する画面のキャンバス領域を、準備済みの画像が覆っていること」を確認してから切り替える。初回の未計算画像には準備時間を認め、ホストが読み込み状態を明示できるようにする。

メモリは、資源を作る前に予算を予約する。読み戻し用の枠はストローク開始前に確保し、小さいバッファで順に読み戻す。全タイルの読み戻しが成功してから、既存の一つの編集として確定する。

初期実装の決定事項：

- readback プールは device ごとに **1 本**。2 本による並列化は後続の計測対象とする。
- 粗い画像は、公開中と生成中の最大 2 世代を保持する。生成要求は最新の 1 件へ集約する。
- 最粗の生成は、最初の可視表示の後から始める。メモリ圧力の発生だけを開始条件にしない。
- 初期表示に必要な処理は、アイドル先読みとは独立して進める。
- 正規タイル、合成順、RGBA16F、MSAA、縮小の算術、保存形式を維持する。
- 完全な総 VRAM 上限を保証する、という API 契約は作らない。

新しい CPU 描画器、全画面の履歴テクスチャ、派生画像の OPFS キャッシュ、キャンバス外タイルの省略は導入しない。

## 2. 保証すること・保証しないこと

| 対象 | 保証 |
|---|---|
| 初回の未準備状態 | preparing を報告し、不完全なキャンバスを「準備完了」として公開しない |
| 有効な surface 上でのパン・ズーム | 新しい画面を覆える画像がなければ、最後の公開済み表示を維持する。短時間の追従遅延を認める |
| 粗い画像の利用 | 同じ文書の画像だけを利用する。古い更新番号の画像を使う場合は、その状態を診断で区別する |
| 詳細タイルへの更新 | 準備済みの透明タイルも領域全体を置き換え、古い絵を透かさない |
| エンジンの GPU 予算 | 定義した要求サイズの課金量について、確保済み・退役中・確保待ち・予約の合計を設定予算以下に保つ |
| ストローク確定 | readback の途中失敗・確定前キャンセルで、文書・Undo/Redo に一部だけ反映しない |
| GPU 喪失 | 失われた epoch の資源・通知を再利用せず、確定済み CPU 文書から復旧する |

以下は保証しない。

- 新しい文書を、計算も転送もせず即座に正確に表示すること。
- 任意の文書、端末、GPU に対する初回表示時間の固定上限。
- document open、device loss、surface 再生成中に、常に前の GPU 画像を残すこと。その期間は preparing として扱う。
- すべての中間画面が最新 revision の全解像度画像であること。
- ブラウザ、driver、shader compiler、Surface、WebGPU/wgpu内部の転送staging、他アプリを含む総物理メモリ使用量。
- 予算内なら OOM や device loss が起きないこと。
- CPU commit 前の未確定ストロークが、Worker 強制終了後も復旧できること。

WebGPU の limits は個々の資源・バインディングなどの制限で、総 VRAM 予約権ではない。したがって本書では「厳密な上限」を、エンジンが API に要求するサイズに基づく課金量に限定する。実際の物理使用量や確保失敗は、別の診断・エラー処理で扱う。[GPUSupportedLimits](https://developer.mozilla.org/en-US/docs/Web/API/GPUSupportedLimits)

## 3. 現行コードで確認した問題

| 箇所 | 現状 | 本書で変更すること |
|---|---|---|
| [renderer.rs](../../crates/alto-gpu/src/renderer.rs) の render / cache_slot | 空きが少ない、または退避すると cache_pressure を立てる | 初回表示・表示継続の必須仕事と、任意の先読みを区別する |
| [cache.rs](../../crates/alto-gpu/src/renderer/cache.rs) の overview_step | 子を保持して一枚ずつ最粗を作る | 算術と分割を維持し、公開中の世代と生成中の世代を分ける |
| renderer.rs の render | 不足時は別レベルのキャッシュを背景に描く | 公開前にキャンバス領域の被覆を確認する |
| [worker.ts](../../packages/alto-sdk/src/worker.ts) の schedulePrefetch | frameSettled が false なら先読みを進めない | 初期準備の必須仕事を、この条件に依存させない |
| renderer.rs の finish_stroke | 変更タイルごとに 256 KiB の readback buffer を作り、全件まとめて map する | 予約済みの 1 本へ小分けコピーする |
| renderer.rs の read_tiles | 要求枚数ぶんの buffer と戻り値 Vec を作る | GPU バッファはプール容量で分割し、CPU 戻り値も別途 admission する |
| worker.ts の EXPORT_BATCH | 固定 64 枚 | Rust が返す有効容量に合わせる。Rust 側でも再検証する |
| [memory.rs](../../crates/alto-gpu/src/renderer/memory.rs) | 描画資源と退役を推定する | readback、確保待ち、用途別予約を追加する |
| [lib.rs](../../crates/alto-wasm/src/lib.rs) の end_brush | Vec の全読み戻し後に put_tile、単一 transact、journal_record | 小分けステージングを追加し、CPU commit と永続化の境界を明示する |

### 3.1 B6 の読み戻し規模

基準シーンは固定配列 193 MiB、ストローク 389 枚 × 0.75 MiB = 291.75 MiB。readback は 389 枚 × 0.25 MiB = 97.25 MiB。

~~~text
193 + 291.75 + 97.25 = 582 MiB
~~~

これは同時確保サイズからの推計で、driver VRAM の実測値ではない。現行の B6 フレーム報告は finish_stroke 内部を観測しないので、この値を既存の gpuBytes 測定値と混同しない。

本案の 8 MiB プールなら、同じ基準計算は 492.75 MiB。ここに一時コマンドなどの課金量が加わる。492.75 MiB は目標計算値であり、本書作成時点の実測結果ではない。

## 4. GPU 予算の統一

### 4.1 課金式

~~~text
A = 有効な確保済み資源の要求サイズ
R = 退役済みで、回収条件をまだ満たしていない課金量
P = 確保処理中の予約量
Q = 用途に確保したが、まだ資源に変換していない予約量

budgetUsed = A + R + P + Q
budgetUsed <= configuredBudget
~~~

A/R/P/Q は排他的な状態で、同じ割当てを二重に加えない。GPU 完了を待たずに同じ queue で再利用する既存の資源は、新規確保ではない。

GPU buffer は実際に要求した容量と alignment を計上する。readback の 256×256 RGBA8 は bytesPerRow=1024、1 枚 262144 bytes。一般化する場合は行 pitch の切り上げも含む。texture は format、寸法、全 mip、layer、sample count に基づく一貫した課金式を使う。非公開の driver 配置は推定に混ぜない。

既存の四区分に readback を追加する。

~~~text
gpuBytes = fixed + effects + stroke + vector + readback
~~~

各区分にはその用途の退役分も含む。liveFixed、retiredFixed、retired は非加算の部分集合を維持する。pendingAllocationBytes、reservedBytes、budgetUsedBytes を別診断にし、予約を既存の gpuBytes へ重複加算しない。表示名は「GPU 管理値」とし、実測 VRAM と表示しない。

### 4.2 管理単位

既存 memory.rs に device 単位の BudgetGovernor を拡張する。AllocationId、用途、要求サイズ、状態、最終 submission を管理する。

- 確保前に reservation を取得し、P へ移す。
- 確保成功・検証後に A へ移す。失敗した handle は利用しない。
- ownership を Renderer から ReadbackSession へ移しても課金は残す。self.stroke.take() によって数から消える構造にしない。
- wgpu handle の clone は同じ AllocationId を参照し、clone ごとに加算しない。
- encoder 内の参照、queue 書き込み待ちを pin する。
- 全 consumer を submit または破棄してから destroy する。
- R は既存の完了台帳で一度だけ回収する。失われた device は epoch ごと破棄する。

短いロックまたは単一 Worker の更新で台帳を操作し、await をまたいで台帳のロックを保持しない。完了 callback は Renderer を借用しない。native の Send 互換性を維持する。

### 4.3 readback と表示用の予約

初期プール容量 C は次を採用する。

~~~text
T = 262144 bytes
C = T * floor(min(8 MiB, configuredBudget / 16) / T)
poolCount = 1
~~~

| GPU 予算 | 現行の固定配列＋scratch | readback 容量 | 1 バッチの最大枚数 | 両者の合計 |
|---|---|---|---|---|
| 64 MiB | 32 MiB | 4 MiB | 16 | 36 MiB |
| 128 MiB | 64 MiB | 8 MiB | 32 | 72 MiB |
| 256 MiB | 128 MiB | 8 MiB | 32 | 136 MiB |
| 512 MiB | 193 MiB | 8 MiB | 32 | 201 MiB |

表は max_texture_array_layers が 512 以上の場合。256 上限の端末では実際の配列容量で再計算する。プール導入後の待機時確保量は、既定設定でも従来の 193 MiB から増えることを明示する。

C は Renderer の準備完了前に予約・確保する。OOM 時の縮小後も最低 1 タイルぶんを保つ。プールを資源として計上した後、その予約 Q は 0 にする。

表示用コマンドには独立の WorkReserve を設ける。初期値は 1 MiB を上限とし、実際の present に必要な descriptor/command buffer サイズを事前計算して admission する。この値は driver 用の安全余白ではない。必要量が収まらなければ確保前に失敗を返す。

WorkReserve からの確保は Q→P→A/R と移し、回収後に Q を戻す。stroke・任意先読みはこの枠を使えない。資源を再利用できる場合には新しい枠を要求しない。実装時に present の最大要求量が上限に収まることをテストする。

### 4.4 admission と backpressure

すべての engine-owned create_buffer/create_texture を同じ予算判定へ接続する。M5、MRT8、brush、present、readback の経路を別々の計算式にしない。部分導入中は「全要求サイズの上限保証が完成」と報告しない。

- 新規確保なしの順序付き再利用：そのまま進める。
- submit 待ちの pin が原因：先に submit または encoder を破棄する。
- R の回収待ち：対象の完了通知で再開する。busy loop は行わない。
- 現在の必須作業自体が収まらない：既存 Budget エラーを返す。正規タイルを省略しない。
- 空きのない任意先読み：中断し、ユーザー操作を優先する。

## 5. readback プールとストローク確定

### 5.1 所有権と状態

新規 renderer/readback.rs に ReadbackPool と ReadbackSession を置く。Pool は device 単位、Session は一度に 1 件。stroke、tile export、診断読み戻しが共用し、同時実行で本数を増やさない。

~~~text
Idle → CopyEncoded → Submitted → Mapping → CpuRead → Idle
                         ↘ Failed / Cancelled → Retired
全状態 → DeviceLost（旧 epoch を破棄）
~~~

Mapping 中の buffer は GPU コピーに使わない。必要な範囲を CPU が読み、mapped view を破棄して unmap した後に Idle へ戻す。mapAsync の完了を、他 buffer や後続処理すべての完了と解釈しない。[GPUBuffer の mapping 契約](https://gpuweb.github.io/types/interfaces/GPUBuffer.html)

Pool の再作成は旧 allocation の退役も予算に含める。本設計では処理途中の拡張をしない。

### 5.2 読み戻し手順

1. beginBrush で、正常な readback Pool、最小の確定処理用予約、文書・layer・revision を確認する。
2. brushInput の追加タイルも、Pool と WorkReserve を残して admission する。同時に変更タイル総数から確定用 CPU 予約を増やし、後から「確定する空間がない」とならないようにする。予約を増やせない入力バッチは追加描画前に拒否し、既に受理したストロークを部分的に自動確定しない。
3. endBrush で対象世代を固定し、GpuStroke を ReadbackSession へ移す。台帳上の stroke 課金はそのまま残す。
4. BTreeMap の既存の順で最大 floor(C/T) 枚を Pool の各 offset へコピーし、submit する。
5. 対応する mapAsync を待ち、各 tile を CPU ステージングへ渡す。生の TileData は現在のバッチを超えて蓄積しない。
6. mapped view を破棄し、unmap する。同じ Pool を次のバッチに再利用する。
7. 読み戻した stroke tile は、後続 consumer がないことを確認して順次退役できる。文書の before hash は別途保持する。
8. 全件成功後に、全 after hash と元 revision を確認し、一回の既存 transact で確定する。
9. 成功・失敗・キャンセルのすべてで、tool の sampler/layer/operation gate を解除する。

各ステップはイベントループへ戻れるようにする。readback のための待機は許すが、通常描画の全 submit に GPU 完了待ちを追加しない。nativeではmap待機中も、Sessionが持つDeviceのcloneから非ブロッキングpollを進められるようにする。WorkerがRuntimeを借用中でも、別のRuntimeメソッドを呼んでpollしない。WebGPUではブラウザのイベントループによるcallback進行を使う。

既存の public Rust finish_stroke() は互換 wrapper として残し、同じ Pool を利用する。Vec 全件を返す wrapper は戻り値の CPU 予算を先に確認する。WASM の end_brush は、全生タイルの Vec を作らない Session 経路へ移す。

### 5.3 CPU ステージング

Runtime/Engine 側に PendingStrokeCommit を設ける。保持するのは文書 identity、元 revision、layer、mask/content の区別、before/after hash、変更キー、GC root、CPU 課金である。GPU 依存を alto-engine へ追加しない。

現在の Engine::put_tile は raw TileData を decoded cache にも入れる。そのままバッチごとに呼ぶと、生タイルが CPU 側に積み上がり得る。新しいステージング入口では、既存方式で encode/hash した object を保管し、raw cache への全件挿入を避ける。生成した hash は、transact の maintain が済むまで GC root として保持する。

readback バッファ、現在の raw バッチ、encode の scratch、全 encoded objects、op metadata、journal の組立て・受け渡しによる重複を課金する。圧縮率が高いという仮定で予約しない。既存 LZ4/コンテナの最大出力サイズを checked arithmetic で計算し、割当て失敗を通常のエラーへ変換できる入口を使う。

初期の確定処理用追加 CPU 論理予算は 512 MiB を上限候補とする。389 枚の incompressible データと一時重複を受け入れられるか、実装時の正確なコピー回数で検証する。上限は検証前の設計値で、テストに合わなければコピー削減か明示設定で見直す。

最大出力サイズと同時コピー数から finishCpuReservation(N, C) を定義し、brushInput の前に増分を予約する。コピー数を推測で固定せず、encoded store、journal の entries、完成した record、受け渡し buffer の所有期間を調べて上界を証明する。未参照 object が回収待ちの間は、その logical charge を早く解除しない。

このガードは確定処理が追加で保持する量を制限する。既存 Engine の全 RAM、WASM linear memory の確保済み高水位、JS の GC 時期を厳密に制限するという契約にはしない。

OPFS への一時 spool は初期実装で採用しない。CPU 予算不足は文書へ適用する前に検出し、理由を返す。キャンバス外も含む正規タイルを捨てて処理を通さない。

### 5.4 commit と永続化の境界

| 境界 | 失敗・キャンセル時の結果 |
|---|---|
| 読み戻し／ステージング中 | 文書 digest・revision・Undo/Redo は開始前と同じ。未参照 object は root を解除して回収可能にする |
| transact の事前検証 | 全 after object の存在、before 値、元 revision を確認。不一致なら無適用で終了 |
| 単一 CPU transact | ストローク全体を一回で反映。バッチごとの transact は行わない |
| CPU commit 後の journal 組立て失敗 | commit 済み状態を SDK mirror へ反映し、永続化失敗として編集を凍結する。成功した CPU 編集を「未適用」と報告しない |
| journal の OPFS 書込み失敗／応答喪失 | 既存の storage failure・緊急 export・再読込の契約を維持する |
| durable の確認 | 既存 durable() の結果で判断する。endBrush の応答だけで永続化済みと見なさない |

現行 journal_record は transact 後の状態から記録を作る。そのため「すべてのエラーで CPU 文書が必ず元のまま」とは書かない。Runtime の内部結果を BeforeCommitFailure と Committed の二種類に分け、Committed は patch と journal の成否を別々に持つ。

Committed の journal 作成に失敗しても、Worker は最新 patch を通知してから storage failure へ移る。既存の endBrush 成功が意味する CPU 確定を維持し、durable は失敗させる。エラーを理由に同じストロークを自動再適用しない。Undo を後から実行して「ロールバック済み」とする方式も採用しない。

mask brush も、一回の SetMask と同じ境界を使う。CPU commit の直前でキャンセル受付を閉じ、commit 後のキャンセルは「既に確定」を返す。

### 5.5 キャンセル・GPU 喪失・再入防止

WASM の async endBrush が Runtime を借用している間、別メッセージから Runtime.cancelBrush / tick / resize を呼ばない。

独立所有の CancelHandle を操作開始前に作り、Worker はその handle のフラグだけを更新できるようにする。Session は各バッチ境界と CPU commit 直前にフラグを読む。native でも安全に共有できる所有権を用い、同じ Runtime への再入を作らない。

map 待ちのキャンセルは、所有している buffer の map を安全に終了・解除してから退役する。遅延 callback は SessionId と deviceEpoch を照合し、次の Session の完了や Pool の Idle 化に使わない。

map の期限は操作ごとの設定・診断とする。期限超過時は Session 内で map を解除し、確認された device loss または必要な明示的 device 破棄により epoch を終える。未解除の map を残して新しい操作を開始しない。map の失敗だけで、原因を一律に device loss と決めつけない。

確定前の GPU 喪失では部分 commit しない。確定済みの CPU 文書は既存の経路で復旧する。Worker 強制終了時の未確定ストローク復元は別の機能で、本設計では保証しない。

### 5.6 export・診断読み戻し

- Runtime.readbackCapacity() で現在の容量を Worker に通知し、固定 EXPORT_BATCH=64 を廃止する。
- Renderer.read_tiles でも容量を検証・分割する。古い Worker の要求枚数を信用して大きな GPU buffer を作らない。
- 既存 readTile/readTileLevel は指定レベルの従来画素を返す。表示用の一時的な粗い LOD を export に流用しない。
- 複数タイルを結合した Vec を返す互換経路は、CPU 戻り値の枠も確認する。枚数の積・offset は overflow を検査する。
- exportImage は全画像 Uint8ClampedArray、WASM/JS の受け渡し、Canvas、拡縮先、encoder に渡すデータの要求量を事前評価する。readback だけ小さくして「全 export の RAM が一定」と報告しない。
- 予算に収まらない全画像 export は、現在の 64M pixel 条件だけで通さず、確保前に理由を返す。PNG 等のストリーミング encoder は後続案とする。
- 複数 export や診断 readback が来ても Pool を増設しない。待ち行列は有界にし、同じ Runtime へ並列に入らない。

## 6. 表示の公開条件

### 6.1 世代と所有権

PresentationState は次を区別する。

| 情報 | 内容 |
|---|---|
| DocumentSessionId | open/create/recover に対応する文書セッション |
| SceneGeneration | revision、live stroke preview epoch、描画設定の世代 |
| DeviceEpoch | device の作成ごとに新しくする |
| SurfaceGeneration | surface size/DPR/再構成の世代 |
| ViewVersion | 入力された最新 viewport |
| PresentedViewVersion | 実際に公開した viewport |
| ActiveOverview | 同じ文書の最後の完成済み粗い画像。更新番号と canvas bounds を保持 |
| BuildingOverview | 最新要求の生成中画像。完成・世代照合前には公開しない |

現在の CacheKey は level/x/y だけである。表示保持用の所有者には generation を加え、invalidate により正規キャッシュを無効化しても、ActiveOverview の lease を誤って解放しない。stroke preview epoch が変わった場合も、途中の子を異なる世代で混ぜて親を公開しない。

生成要求は一件に集約する。新しい edit が来たら、古い未提出 encoder を破棄できる。提出済みの consumer は既存の退役・pin 契約で守り、GPU がまだ使う資源を先に壊さない。

### 6.2 状態遷移

~~~text
Preparing → ReadyExact
Preparing → ReadyCoarse → Refining → ReadyExact

ReadyExact / ReadyCoarse
  → 更新要求
  → Holding（最後に公開した view を維持）
  → ReadyCoarse または ReadyExact

文書交換 / DeviceLost / 保持できない Surface 再生成
  → Preparing

予算に収まる被覆を作れない
  → Failed（理由を通知）
~~~

Preparing はホストが読み込み表示を出すための状態で、任意の背景を正しい文書画素と扱うものではない。文書を交換するときは、別文書の旧画像を暫定表示に流用しない。

初回の公開条件は次のいずれか。

- 実際に公開する view の、キャンバスに交わる領域をすべて、準備済みの同世代詳細タイルが覆う。
- その領域を覆う、同じ文書の完全な粗い画像がある。

キャンバス外は背景として既知なので、文書タイルの欠落とは区別する。透明画素も準備済みの画素であり、欠落ではない。

### 6.3 被覆の証明と present

CPU 側に CoverageCertificate を作る。対象の document/generation/device/surface/view、使用するタイル・slot generation、被覆領域、古い fallback を含むかを持つ。

rotated viewport のキャンバス内領域を覆えることを、保守的な required tile 集合で検査する。alpha を走査して準備状況を判定しない。新しい view を覆えなければ、新しい view の部分 present を行わず、既に公開した画面を維持する。

CoverageCertificate 取得から present encoder の submit まで、使用 slot を pin する。最後に世代と slot generation を再照合する。GPU 完了まで全 slot を使用禁止にする fence は追加しない。

現行の [present.wgsl](../../crates/alto-gpu/shaders/present.wgsl) は、tile の premultiplied RGBA と市松模様を合成し、alpha=1 を返す。この不透明な領域置換を維持すれば、新しい透明タイルの下から古い画像が出ない。初期実装に追加の GPU coverage texture は不要である。

ActiveOverview を背景に、準備済み詳細タイルを後から描く。異なる revision の fallback が残る場合は hasStaleFallback=true を報告する。保存・書出し・hit test は CPU 文書と指定世代を参照し、fallback の更新番号を正規データに採用しない。

Surface の再生成で保持画面が失われ、使える fallback もない場合は Preparing に戻す。追加の全画面 GPU 履歴コピーで隠す方式は採用しない。

### 6.4 表示保持中の座標と入力

現行 [viewport.ts](../../packages/alto-sdk/src/viewport.ts) の screenToDocument/documentToScreen は、要求済みの view を使う。[tools.ts](../../packages/alto-sdk/src/tools.ts) の付属pointer adapterもこの変換を呼ぶ。長いHoldingを導入するだけでは、見えている位置と描画座標がずれる。

既存の viewport.set/get/zoomAt および座標変換の同期的な意味は維持する。これらを無言で「最後に描いたviewの変換」に変えない。別の read-only PresentationSnapshot に presentedViewport、presentedViewVersion、surface世代、interactionReadyを公開する。

付属pointer adapterとPlaygroundは次の条件を守る。

- 新しい画面入力は、要求viewと公開済みviewが一致し、対象領域が現世代で準備済み、surfaceも一致する場合だけ受け付ける。
- Preparing/Holdingや古いfallbackだけの期間は、表示更新中を明示し、ブラシ・選択・変形・hit testの新規画面操作を始めない。未準備の入力を黙って蓄積して後から別座標へ再生しない。
- パン・ズーム要求は最新値へ集約する。付属adapterで描画ストローク中の場合は、その終了またはキャンセルまでviewportの公開切替を保留する。
- プログラマティックな文書座標の編集APIは、画面入力とは区別して既存の直列操作契約を維持する。自作ホストにもPresentationSnapshotを使う条件を文書化する。

interactionReadyの更新は、対象presentのGPU処理完了を確認したackと世代照合に基づく。これはブラウザとの論理的な公開状態であり、OSの実際のscanout時刻まで保証するものではない。

### 6.5 最粗の生成とスロット予算

既存 overview_step の子→親の縮小と、1 スライスずつの進行を利用する。全原寸タイルを一つの巨大な合成へ集めない。

4K では最粗レベルは 4、完成画像は 1 タイル。Active と Building の 2 枚で表示配列の 2 slot を使う。既存配列の容量内で使うため、fixed のバイト数に 2 枚を重複加算しない。

深さ L の DFS 作業枠は、未完了の兄弟を各階層で最大 3 枚保持する保守的な上限として、初期設計では 1+3L slot を用意する。実装時に出力 slot の lifetime を検証し、この上限で足りることをテストする。L=4 なら 13 slot。level 12 が複数枚必要なキャンバスでは、完成粗画像の枚数 N に応じて両世代分を予約する。

slot admission は、Active/Building、DFS 作業枠、公開中の詳細領域、生成中の詳細領域の lease の和集合で行う。同じ slot を二重に数えない。

- ActiveOverview が完成済みなら、旧詳細を退避して粗い表示へ戻すことができる。
- ActiveOverview がない間は、公開中の詳細を保持し、予約した作業枠で粗い画像を作る。新 view のために保持分を壊さない。
- 低予算で要求 LOD の詳細が収まらない場合は、表示用の admittedLevel を粗くする。requestedLevel と degradedByBudget を診断で報告する。
- 正常予算で requestedLevel が収まる場合は、従来どおりそこまで refine する。
- 両世代と最小作業枠すら収まらなければ、表示開始前に Budget エラーを返す。キャンバスの一部を黙って省かない。

粗い admittedLevel は表示品質の選択であり、正規画素や指定レベルの readback を変更しない。API の pixel parity 試験と、表示が一時的に粗いことの試験を区別する。

### 6.6 スケジューラと待機

仕事を Visible、RequiredOverview、SpeculativePrefetch に分ける。

初回は Visible を先に進め、公開可能になったら最新の RequiredOverview を進める。保持中の詳細だけでは新しい view へ移れない場合は RequiredOverview を必須仕事として進める。通常の任意先読みはその後にする。

必須仕事は tick または専用の短い仕事スライスから進め、frameSettled=true を条件にしない。現行 schedulePrefetch の条件をそのまま使うと、Preparing の完了に必要な仕事が起動しない循環待ちになる。

waitFrame も同じ進行関数を使う。再開時には pending viewport/resize と ViewVersion を取り込み、古い view の完成だけを延々待たない。文書変更など、操作順を守る必要がある要求は既存の直列 gate を維持する。

一つのスライスで進める leaf/parent や入力数を有界にし、タスク間で入力を処理する。既存 JOB_SLICE_MS=1.8 は目標として維持するが、単一の重い M5/tessellation 呼出しまで必ず1.8ms以下に収まるとは保証しない。

readback や slot の Blocked は、完了・回収・入力更新などの明示的 wake 条件を持たせる。進捗のない自己再スケジュールをしない。

## 7. 確保失敗と device epoch

Pool 作成・再作成は OOM と validation を分けて扱う。固定済み wgpu 30.0.1 の API では、push_error_scope が返す ErrorScopeGuard を pop() して Future を得る。旧バージョンの device.pop_error_scope() を前提にしない。

ScopeGuard は !Send/!Sync なので、資源作成直後に scope を pop し、guard を await をまたいで保持しない。Renderer に guard を格納して native Send 互換性を壊さない。これは手元の固定版ソースで確認した実装条件である。

- OOM：invalid handle を destroy し、P を整理して Pool 容量を半分にして再試行できる。
- 最小 1 タイルまでの試行回数を上限とする。32枚開始なら最大6回。
- validation：縮小リトライで隠さず、実装の不整合として詳細を返す。
- device loss：旧 epoch の callback、map、reservation、資源を新 epoch から切り離す。
- 正規文書は CPU/保存済みデータから再構築する。旧 queue の完了が永久に来ることを回復条件にしない。

error scope の成功は、将来の物理メモリ確保や device の健全性を永久に保証しない。[WebGPU の確保失敗・device loss の説明](https://gpuweb.github.io/gpuweb/explainer/)

## 8. 診断と API 互換性

追加候補は以下。名前は実装時の内部型で確定する。

| 分類 | 診断 |
|---|---|
| GPU | readback、pendingAllocationBytes、reservedBytes、budgetUsedBytes、budgetLimitBytes、operationPeakBytes |
| readback | capacityBytes、inFlightBytes、mappedBytes、batchCount、completedTiles、waitReason、sessionId |
| 表示 | phase、documentSessionId、sceneGeneration、viewVersion、presentedViewVersion、requestedLevel、admittedLevel |
| 表示の品質 | coarseCoverageComplete、targetCoverageComplete、hasStaleFallback、degradedByBudget、holdReason、interactionReady |
| 操作の補助 CPU | stagingBytes、rawBatchBytes、encodedPendingBytes、journalReservedBytes、exportRequestedBytes |

WASM の既存 FRAME_FIELDS は位置を変えず末尾に追加する。enum は内部数値として渡し、Worker が診断へ復号する。display.* 等を追加する際は decodeFrame に明示グループを足し、prefetch へ混入させない。

GPU 完了 callback や readback ステップで最大値を台帳に保持する。フレーム間で終わる endBrush のピークも operationPeakBytes として取得し、フレームごとの最大サンプルだけで評価しない。

統計値の追加は unstable 診断を基本とする。表示状態とPresentationSnapshotはホストの正しい入力受付に必要なので、P4で最小のtypedなread-only公開面を追加し、型・API snapshot・組み込み例を同時に更新する。既存Workerのstatus.readyと表示準備完了を同じ状態として扱わない。ABI/peer protocol 変更が必要な場合は実装 PR で明示し、protocol=4 を無条件に維持できると決めつけない。

waitFrame/settle は現行と同じく、対象 view の admittedLevel が完成し、必要な present/GPU 待機を終えてから解決する。画面を粗い fallback で覆えたことだけで pending=0 にしない。全キャンバスの裏側の overview 生成完了は waitFrame の必須条件にしない。

## 9. 実装を分ける単位

| 段階 | 変更 | 完了条件 |
|---|---|---|
| P0 | 現行基準と操作ピークの診断を記録する | B6のフレーム値と確定内部の確保を区別して再現できる |
| P1 | memory.rs の共通予約・AllocationId・readback用途・WorkReserve | 全 allocation 経路の台帳が合い、GPU予算式の不変条件をテストできる |
| P2 | readback.rs の1本Pool、read_tilesの分割、exportの容量連携 | GPU読み戻し量がタイル総数で増えず、従来画素と一致する |
| P3 | PendingStrokeCommit、CPU補助予算、単一commit、キャンセル・失敗境界 | 途中失敗と最終バッチ失敗で部分commitがない。journal失敗時のmirrorが一致する |
| P4 | PresentationState、generation、被覆証明、Work scheduler、入力gateとtyped公開面 | 初回preparingが進み、旧view保持・新view公開・画面座標の一致をテストできる |
| P5 | Active/Building overview、作業slot予約、表示LOD admission | メモリ圧力前から粗画像を作り、パン・編集・低予算で欠落を公開しない |
| P6 | CI・Metal計測・配布物の照合・結果文書 | 受入表を実測で満たし、ソースとWASM/SDKが一致する |

P2 を実装しただけでストローク確定の原子性まで完成と扱わない。P4/P5 の公開条件を切り替える前に、P1 の slot/資源予約を完成させる。

主な変更先：

- crates/alto-gpu/src/renderer/memory.rs：要求量の予約、割当て、退役、ピーク記録。
- crates/alto-gpu/src/renderer/readback.rs（新設）：Pool、Session、mapとcancelの所有権。
- crates/alto-gpu/src/renderer/presentation.rs（新設）：表示状態、世代、被覆証明。
- crates/alto-gpu/src/renderer/cache.rs：overviewの所有・作業slotと退避。
- crates/alto-gpu/src/renderer.rs、renderer/m5.rs、renderer/vector.rs および vector/：共通quotaと公開条件への接続。
- crates/alto-engine/src/lib.rs：ステージング用object登録・GC root・補助CPU予算。GPUへの依存は追加しない。
- crates/alto-wasm/src/lib.rs：Session駆動、endBrushの結果境界、診断。
- packages/alto-sdk/src/worker.ts / protocol.ts / unstable.ts / engine.ts / viewport.ts / tools.ts、Playground：必須仕事、動的export batch、診断、失敗時のmirror、PresentationSnapshotと入力gate。
- tests/e2e/browser-tests.js / m5-tests.js / m6-tests.js / embed-tests.js、必要なRust単体テスト。
- verification/build.json、SHA256SUMS、両配布WASM、SDK dist：実装段階で再生成・照合する。

独立した巨大な新レンダーグラフや、複数の予算管理系を増やさない。既存の memory/cache を拡張し、readbackと表示公開の責務だけを小さいmoduleへ分ける。

## 10. 受入テスト

下表は実装後に必要なテストであり、現在通過済みのテスト一覧ではない。

| ID | 条件 | 合格条件 |
|---|---|---|
| M01 | 64/128/256/512 MiB、全allocatorの作成・移動・退役・失敗 | 全イベント直後の A+R+P+Q が予算内。subsetを二重加算しない |
| M02 | self.stroke.take() / M5 take / Sessionへの所有移動 | 所有者が変わっても課金は減らない |
| M03 | バッファ・テクスチャclone、二重cancel、二重callback | AllocationIdごとの加算・回収は一回 |
| M04 | 全常駐量が予算近く、Poolは既にIdle | 新規確保をせずreadbackを開始できる |
| M05 | OOM、validation、古いepochの完了通知 | OOMのみ有界な縮小、validationを隠さず、旧通知は新台帳へ無影響 |
| R01 | 1/31/32/33/389枚、64MiBの16枚境界 | 全正規キー・hash・RGBAが現行参照と一致、Pool容量が枚数に依存しない |
| R02 | mapped中の再利用・別Sessionの侵入 | 不許可または有界待機。解放前にcopyしない |
| R03 | 第1/中間/最終バッチでmap失敗・cancel・device loss | commit前のdigest、revision、Undo/Redoが不変。toolを解除できる |
| R04 | 第1バッチ生成後にGCを実行 | stagingのhashが消えず、成功後の文書・Undoが同じ |
| R05 | CPU commit直後のjournal組立て失敗・OPFS失敗・応答喪失 | 全CPU編集とmirrorを一致させ、保存失敗を通知。再送で二重適用しない |
| R06 | 選択付き・変形付き・mask brush・完全にcanvas外のtile | 全キー/hash、移動後の画素、Undo/Redo、ZIP、GPU再構築が一致 |
| R07 | 巨大export・同時export・整数overflow・最低1枚にも足りない予算 | 確保前に有界に待機またはエラー。Poolを増設しない |
| D01 | 空cacheで初回open、可視仕事を意図的に遅らせる | 最初はPreparing。被覆前の部分presentを行わず、後から必ず進捗する |
| D02 | frameSettled=false、任意prefetch無効 | 必須仕事が進む。waitFrameが循環待ちしない |
| D03 | 最粗未完成の連続zoom/pan/rotation | 公開済みviewを保持し、最新viewだけを公開。表示追従の遅延を診断できる |
| D04 | 完成粗画像あり、詳細tileを意図的に遅延 | 粗画像がcanvas内を覆い、完成詳細へ順次更新する |
| D05 | 旧粗画像の白領域を消しゴムで透明にし、新詳細が完成 | 旧白が透けず、市松模様になる。透明tileを未準備に数えない |
| D06 | 複数文書open、編集連打、preview epoch更新 | 別文書や異なる世代の子を混ぜない。公開前の世代照合が働く |
| D07 | surface resize/DPR変更、GPU喪失 | 古い証明を無効化し、利用可能画像がなければPreparingを通知 |
| D08 | 42 display slots、L=4、L=12、複数最粗tile | slot和集合が容量内。必要なら表示LOD低下を明示し、不能なら開始前エラー |
| D09 | ActiveとBuildingの競合・cache退避・生成cancel | Activeと未提出consumerを壊さず、世代交換後に一度だけ解放 |
| D10 | Holding中のpointerdown、stroke中のzoom、present ackの遅延 | 見えていないviewで描画・hit testせず、入力を別座標へ再生しない。同期viewport変換の既存契約も維持 |
| A01 | Rust native、WASM、SDK組み込み | Send互換性、内部ABI、公開型snapshot、promise/キャンセルの順序を維持 |

### 計測とリリース判定

- SwiftShader：上の論理的保証と既存の機能・M5/M6/M7/M8・組み込み・画素一致を合格条件にする。
- Metal：B6を再計測し、描画中・確定中・確定後の全課金区分と予約を記録する。frame最大値の寄せ集めでピークを作らない。
- 8MiB/32枚は初期設計値。endBrush時間、batch数、入力処理までの待ち、map時間、CPU staging高水位を現行と比較する。メモリ削減だけで処理時間の大幅悪化を隠さない。
- 固定msの初回表示保証は設けず、Preparing時間、初回被覆まで、requestedLevel到達まで、view追従遅延を分けて記録する。
- B6の合計が常に492.75MiBになることを合格条件にしない。一時資源・退役・予約を含む全イベントの不変条件と、画素・履歴一致で判定する。
- ソース、fresh release WASM、SDK dist、build metadata、SHA256SUMSを照合する。
- 結果文書には計算値、エンジン課金値、ブラウザ／OSの観測値を別々に記載する。物理VRAMの観測手段がない環境を0byte扱いしない。

## 11. 後続に回すこと

- 2本のreadbackプールでのcopy/map並行化。採用時は本数ぶんを先に予約し、同じ故障試験を追加する。
- 圧縮済みstagingのOPFS退避と、Worker強制終了前の未確定ストローク復元。
- PNG等の逐次encoder。Canvas依存のJPEG/WebP等と要件を分けて設計する。
- 保存ファイルへ派生previewを入れること。採用時はsnapshotとの対応と破損時の扱いを別途定める。
- 他queue・外部GPU APIとの所有権移譲。
- ブラウザ／driver側の間欠的なGPU喪失そのものの根治。本書の復旧・予算管理で解消済みとは判断しない。

## 12. 参照と解釈

確認日：2026-10-27。本書の採用方式・容量・状態名はAlto向けの設計判断であり、参照資料に同じ実装が記載されているという意味ではない。

- [WebGPU Explainer](https://gpuweb.github.io/gpuweb/explainer/)：GPU/CPUの所有、確保失敗、device lossの背景。Draft Community Group Reportで、規範仕様そのものとは区別する。
- [GPUBuffer API reference](https://gpuweb.github.io/types/interfaces/GPUBuffer.html)：map/unmap/destroyと、bufferごとの完了通知の範囲。
- [GPUSupportedLimits](https://developer.mozilla.org/en-US/docs/Web/API/GPUSupportedLimits)：公開される個別上限。総物理VRAM予算と解釈しない。
- [固定版 wgpu 30.0.1 Device API](https://docs.rs/wgpu/30.0.1/wgpu/struct.Device.html)：実装時に使用する版。ErrorScopeGuard/popおよびSend条件は手元の30.0.1ソースでも確認した。
- [GPU メモリの現行検証記録](../verification/GPU-MEMORY-RESULTS-2026-10-26.md)：本書の基準。ここにある旧テスト成功と、本書の未実装部分の受入完了を区別する。
