# 未実装機能の設計と実装計画

基点：main `bdf26a9`（2026-11-17 書き直し）

作成日：2026-11-04（基点 main `b2036ce`、PR #75 マージ後）。2026-11-17 に、その後の146コミット（堅牢化 A〜F、版を `.alto` の外に持つ F-9 など）に合わせて全13本を書き直した（[書き直しの設計](../2026-11-17-backlog-refresh.md)）。

> **For agentic workers:** 各設計書の項目の「作業手順」を、superpowers:subagent-driven-development または superpowers:executing-plans で1項目ずつ実行する。

**Goal:** 文書に記載された未実装項目のすべてに、今のコードのまま着手できる設計と作業手順を用意する。

**Architecture:** 領域ごとに1文書。各項目は「現状／11-04 からの変化／設計／形式と通信／作業手順／依存」の6節を持つ。本書は全項目の対応表、共通規則、番号の割り当て、文書をまたぐ重なりの解き方、順序、利用者の決定が要る点をまとめる。

**Tech Stack:** Rust / wgpu / WGSL / WASM、TypeScript SDK、Chrome（SwiftShader、Metal）。

状態：設計のみ。本書の作成時点で、ここに挙げた機能のコードは1行も追加していない（QA-1、QA-4、QA-5 は実装済みで、残作業だけを載せる）。工数は相対値で、実測ではない。

**方針（2026-11-04、利用者の決定）：AI と UI 以外をすべて実装する。** AI 機能（SEL-4、AI-2、AI-3）と UI 項目（UI-1〜5）は除外し、設計だけを記録として残す。共同編集は AI 機能ではないので実装の対象とする（COLLAB-1。旧 AI-1）。

## 文書

| 文書 | 領域 |
|---|---|
| [01-color.md](01-color.md) | 色空間、ビット深度、リニア合成、ICC、CMYK、render v4 |
| [02-effects.md](02-effects.md) | 追加効果、自動補正、複数入力、非破壊グラデーション |
| [03-brush.md](03-brush.md) | ブラシ先端、傾き、筆圧カーブ、Undo の深さ |
| [04-selection.md](04-selection.md) | 選択の加工、パスからの選択、SDK公開 |
| [05-image-geometry.md](05-image-geometry.md) | 焼き込み、ラスタライズ、ミップマップ、高度な変形 |
| [06-text.md](06-text.md) | 組版、書式、フォント、文字描画 |
| [07-vector-svg.md](07-vector-svg.md) | グラデーション、破線、SVG、vello |
| [08-versioning-git.md](08-versioning-git.md) | マージ、stash、履歴打ち切り、同期、Git |
| [09-storage-export.md](09-storage-export.md) | 部分読み込み、ジャーナル、書き出し、PSD、外部保存先 |
| [10-runtime-performance.md](10-runtime-performance.md) | スレッド、ネイティブ、表示性能 |
| [11-playground-ui.md](11-playground-ui.md) | Playground、UI部品、フレームワーク包み（除外の記録） |
| [12-collab-ai.md](12-collab-ai.md) | 共同編集（AI機能は除外の記録） |
| [13-verification.md](13-verification.md) | ファジング、環境検証、障害試験 |

## 全項目の対応表

工数：S（1PR・数時間）、M（1〜2PR）、L（複数PR・描画3実装あり）、XL（基盤変更）。段階：1=基盤と小さな独立項目、2=中規模の機能、3=大規模な基盤変更、4=段階3の基盤に依存するか、製品の範囲を広げる項目、除外=実装しない。

| ID | 項目 | 工数 | 段階 | 描画変更 |
|---|---|---|---|---|
| RV4-1 | render v4 の基盤 | XL | 3 | render v4 |
| COL-1 | Display P3 | L | 3 | 表示のみ |
| COL-2 | 16bit | XL | 4 | render v4 |
| COL-3 | F16 / HDR | XL | 4 | render v4 |
| COL-4 | リニア合成 | L | 4 | render v4 |
| COL-5 | ICC カラーマネジメント | L | 3 | なし |
| COL-6 | CMYK、特色 | XL | 4 | あり |
| FX-1 | シャープ | M | 2 | あり |
| FX-2 | 移動ぼかし | M | 2 | あり |
| FX-3 | ノイズ | M | 2 | あり |
| FX-4 | ハイパス | M | 2 | あり |
| FX-5 | ドロップシャドウ | L | 2 | あり |
| FX-6 | 境界線 | L | 2 | あり |
| FX-7 | 自動補正 | S | 2 | なし |
| FX-8 | 複数入力の効果 | XL | 4 | あり |
| FX-9 | 非破壊グラデーション | L | 2 | あり |
| BR-1 | 先端の角度と丸み | M | 2 | あり（ストローク） |
| BR-2 | 画像の先端 | L | 2 | あり（ストローク） |
| BR-3 | テクスチャブラシ | L | 2 | あり（ストローク） |
| BR-4 | 傾きの反映 | S | 1 | なし（ダブ生成） |
| BR-5 | 筆圧カーブ、速度連動 | S | 1 | なし（ダブ生成） |
| BR-6 | Undo の深さの設定 | S | 1 | なし |
| SEL-1 | 色域・連続領域のSDK公開 | S | 1 | なし |
| SEL-2 | 拡大、縮小、境界 | S | 1 | なし |
| SEL-3 | パスからの選択 | S | 1 | なし |
| SEL-4 | 被写体選択 | M | 除外 | なし（12へ） |
| IMG-1 | 属性付きの焼き込み | L | 2 | なし |
| IMG-2 | 任意変形のベクターのラスタライズ | M | 2 | なし |
| IMG-3 | テキストのラスタライズ | S | 1 | なし |
| IMG-4 | コンテンツミップマップ | L | 3 | 表示のみ |
| IMG-5 | 画像編集のGPU高速化 | L | 4 | なし |
| IMG-6 | 遠近法、メッシュ変形、液化 | XL | 4 | CPU確定のみ |
| TXT-1 | ルビ | L | 2 | なし（組版） |
| TXT-2 | 圏点 | M | 2 | なし（組版） |
| TXT-3 | 割注 | L | 3 | なし（組版） |
| TXT-4 | ぶら下げ | S | 1 | なし（組版） |
| TXT-5 | 字下げ、段落ごとの揃え | M | 2 | なし（組版） |
| TXT-6 | 下線 | S | 1 | なし（組版） |
| TXT-7 | 斜体の合成 | S | 1 | なし（組版） |
| TXT-8 | 可変フォント | M | 2 | なし（組版） |
| TXT-9 | カラーフォント | L | 3 | あり |
| TXT-10 | ヒンティング、ステム強調、ガンマ、LCD | XL | 4 | render v4 |
| TXT-11 | 縦組み中のRTL | S | 2 | なし（組版） |
| VEC-1 | 塗りのグラデーション | L | 2 | あり |
| VEC-2 | 破線 | S | 1 | なし（パス展開） |
| VEC-3 | SVG の CSS、画像、文字、外部参照 | L | 2 | なし |
| VEC-4 | vello 本体描画 | XL | 4 | 別経路 |
| VER-1 | 複数の共通祖先 | M | 2 | なし |
| VER-2 | 画素内の自動マージ | M | 2 | なし |
| VER-3 | 文字範囲の自動マージ | M | 2 | なし |
| VER-4 | stash | S | 1 | なし |
| VER-5 | 履歴の打ち切り | M | 2 | なし |
| VER-6 | 分散同期 | XL | 4 | なし |
| GIT-1 | Git 変更の取り込み | L | 3 | なし |
| GIT-2 | rebase、cherry-pick | M | 3 | なし |
| GIT-3 | ブラウザ内 Git | L | 4 | なし |
| STO-1 | 作業文書の部分読み込み | XL | 3 | なし |
| STO-2 | 文書別ジャーナル | S | 2 | なし |
| STO-3 | JPEG、TIFF 書き出し | S | 1 | なし |
| STO-4 | 逐次エンコーダー | M | 2 | なし |
| STO-5 | 保存ファイルへのプレビュー格納 | S | 2 | なし |
| STO-6 | 派生 OPFS キャッシュ | L | 3 | なし |
| STO-7 | ステージング退避、未確定ストローク復元 | L | 3 | なし |
| STO-8 | 最近使ったファイル | S | 1 | なし |
| STO-9 | タイル表の別オブジェクト化 | M | 2 | なし |
| STO-10 | 破損タイルの部分表示 | M | 2 | 表示のみ |
| STO-11 | PSD 往復 | XL | 4 | なし |
| STO-12 | Google Drive などの保存先 | M | 4 | なし |
| RUN-1 | WASM スレッド、SharedArrayBuffer | L | 3 | なし |
| RUN-2 | Compute Worker | M | 3 | なし |
| RUN-3 | `native-viewer` とネイティブ組み込み | M | 4 | なし |
| RUN-4 | readback プール2本化 | M | 3 | なし |
| RUN-5 | 外部 GPU API との所有権移譲 | L | 4 | なし |
| PERF-1 | 可視タイルのバッチ化 | L | 3 | なし |
| PERF-2 | シーン索引 | M | 3 | なし |
| PERF-3 | コールド描画 250 ms | L | 3 | なし |
| PERF-4 | 効果付き文書のジョブ化と先読み | L | 3 | なし |
| PERF-5 | 復旧内部の段階別計時 | S | 1 | なし |
| PERF-6 | 低速環境でのアイドル限定切替 | S | 2 | なし |
| PERF-7 | 重いグループの中間キャッシュ | M | 3 | なし |
| UI-1 | スタンプと指先のドラッグ | S | 除外 | なし |
| UI-2 | レベル補正などの操作画面 | S | 除外 | なし |
| UI-3 | 合成モードの選択肢 | S | 除外 | なし |
| UI-4 | エディタUI部品 | XL | 除外 | なし |
| UI-5 | React、Vue の包み | M | 除外 | なし |
| COLLAB-1 | 共同編集 | XL | 4 | なし |
| AI-2 | 外部画素とマスクの取り込みAPI | S | 除外 | なし |
| AI-3 | 生成塗りつぶし | M | 除外 | なし |
| QA-1 | ファジング（実装済み。残りは入口の追加） | S | 1 | なし |
| QA-2 | Safari、Firefox、Edge | L | 1 | なし |
| QA-3 | 別GPUの実機 | M | 1 | なし |
| QA-4 | 長時間運用（実装済み。残りはアトラス圧迫の試験） | S | 2 | なし |
| QA-5 | タブ強制終了（実装済み。残りは未検証の点の試験） | S | 1 | なし |

### 2026-11-17 に変えた工数と段階

変えたものだけを示す。ほかの項目の工数・段階は 11-04 のまま。

| ID | 11-04 | 2026-11-17 | 理由 |
|---|---|---|---|
| QA-1 | M | S | `fuzz/` の8 target、`fuzz.yml`、`fuzz_regressions.rs` が実装済み。残りは F-9 で増えた入口（`folder`、`decode` の target、薄い `.alto` の seed、TS のジャーナル読み取りの変異試験）だけ（13） |
| QA-4 | M | S | `tests/e2e/soak.js`、F-2（140分）、F-9（薄い保存で4時間）が実装済み。残りはアトラス圧迫の短い試験と再試験の条件（13） |
| QA-5 | S | S（実装済み） | 28点・56ケース（10回連続の記録は27点分）が実装済み。残りは `append-random-kill`、Worker の無言の終了の再現など（13）。工数は変えず、実装済みを明記 |
| STO-2 | M | S | `listRecoveryNamespaces`／`deleteRecovery` は実装済み。残りは `namespace: "auto"`、`label.json`、`MILESTONES.md` の修正だけ（09） |

VER-4 は段階1のままだが、独立項目ではない。manifest の `format` 4 と `local_roots`（新規）を最初に入れる、VER-6、GIT-1 の基盤である（08）。測定で見送りになりうる項目（下の「測定で実装の形が変わる項目」）は、工数を見送らない場合の値で示す。

「旧形式からの移行」は実装しない。リリース済みの形式がなく、移行元がない。最初のリリース以降の形式変更で移行を実装する（共通規則3）。

## 共通規則

全タスクの要件に暗黙に含まれる。

1. **既存の画素を変えない。** `tests/parity/cpu-reference.txt`、`tests/parity/v3-browser.txt`、`tests/parity/text-bake.txt` の既存行は変更しない。既存文書の画素が変わる変更は render v4 として本書の該当節を更新してから行う。v4 の行は別のファイル（`tests/parity/cpu-reference-v4.txt`（新規）、`tests/parity/v4-browser.txt`（新規）。01 の RV4-1）に置く。
2. **描画の3実装。** render v3 の画素を出す機能は、Rust の CPU 参照（`crates/alto-gpu/src/cpu_composite/mod.rs` 以下、試験専用）、JS の参照（`tests/e2e/cpu-composite.js`）、WGSL（`crates/alto-gpu/shaders/effects-q14.wgsl` など）を揃え、Metal、lavapipe、SwiftShader で全バイト一致させる。v3 のシェーダーに浮動小数の演算を入れない（`pool_tests.rs` の `v3_stages_call_no_f32_coordinate_helper` が検査する）。文書の f32 / f64 は、ホスト側で整数へ量子化してからシェーダーへ渡す。render v4 の機能も同じく3実装を揃え、v4 の3実装どうしを一致させる（v3 との一致は求めない。01）。
   - **ブラシは例外（03 参照）。** ブラシは GPU の f32 スタンプと読み戻しが正本で、v3 の整数経路を通らない。検証は `cpu_paint` との ±2 の比較（`tests/e2e/browser-tests.js`）で、`tests/parity/*.txt` にブラシの行は無い。例外の範囲は `brush.wgsl` と `cpu_paint` で確定するタイルだけである。
3. **形式。** 文書の意味を増やす変更は `DocumentMeta::format_version` を上げ、`deny_unknown_fields` の型へ追加する。最初のリリースまでは移行を作らない。番号は下の「番号の割り当て」の回に従い、同じ回の PR は1つの番号を共有する。
   - **format を上げるときは、manifest の `document_format`、読み込みの受け入れ、`gc.rs`、`merge.rs` も同じ PR で変える。** 今は `crates/alto-doc/src/lib.rs` の `DocumentMeta::srgb_u8` と `validate`、`crates/alto-store/src/lib.rs` の manifest の書き出し（`document_format: 5`）と読み込みの照合、`crates/alto-engine/src/versions/gc.rs`、`crates/alto-engine/src/versions/merge.rs` が 5 を固定で持つ。全文書で最初に番号を上げる PR（回 6）が、01 の「共通の設計：形式番号を上げる経路」に従い `alto_doc::FORMAT_VERSION`（新規）と `alto_doc::SUPPORTED_FORMATS`（新規）を入れ、固定の 5 をすべて置き換える。以後の回は `SUPPORTED_FORMATS` に番号を足すだけにする。
   - 文書は使う意味のうち最も新しい回の番号で書く（`Document::required_format`（新規））。新しい意味を使わない文書は 5 のまま書き、今の読み手でも開ける。マージは両方の番号が `SUPPORTED_FORMATS` に含まれれば受け、結果の番号は求め直す。
   - manifest 自体の `format`（2＝完全、3＝薄い）を上げる変更は 08 の「共通の設計：参照の追加と manifest の形式」に従う。
   - format を上げる PR は、ファザーの seed を作り直す（13 の QA-1）。外部バイト列の新しい入口を足す PR は、fuzz の target と seed を同じ PR で足す。
   - Worker との要求を増やす変更は `packages/alto-sdk/src/protocol.ts` の `PROTOCOL` と `crates/alto-wasm/src/lib.rs` の `protocol_version` を上げ、README と RELEASING を同じPRで更新する。
4. **編集の不変条件。** 編集は検証してから適用し、失敗は状態を変えない。可逆 Op の1トランザクションで1回の Undo に対応する。ジャーナル、保存、再オープン、コミット、差分、マージ、GPU喪失後の再構築を通す。保存は完全な `.alto`、`.altoproj` フォルダ、薄い `.alto` と版の置き場（F-9）の3つの形を通す。
5. **上限。** 入力の上限を定数で定め、超えたら原子的に拒否する。作業量に上限のない処理を作らない。堅牢化（A〜F）で入った上限とエラーコード（`doc/verification/ENGINE-HARDENING-RESULTS.md`、`doc/MILESTONES.md`「上限・継続課題」）と矛盾させない。
6. **決定性。** 乱数、時刻、ハードウェア依存の値を文書の画素の計算に使わない。
7. **検証。** 各タスクの完了時に該当する次を実行する。

   ```bash
   sh scripts/check.sh native
   ```

   ```bash
   pnpm --dir packages/alto-sdk build
   ```

   ```bash
   node scripts/verify-browser.mjs --functional
   ```

8. **レビュー。** PR ごとに opencode の読み取り専用レビューを行い、指摘はコードで確かめてから対応する。

## 番号の割り当て

各文書の「形式と通信」は本節の値を使う。値は本書の「実装の順序」に沿って決めた。計画と違う順でマージする場合は、マージの順に番号を取り直し、本節と該当文書を同じ PR で直す。同じ回の番号は、回の最初の PR が上げ、同じ回の後の PR は上げない（最初のリリースまで。リリースの後は PR ごとに上げる）。

### シェーダーのモード番号（`effects-q14.wgsl`）

使用中を実コードで確かめた。`effects-q14.wgsl` の `main` は 8、17〜33 を分岐する。CPU 参照の段の分岐（`crates/alto-gpu/src/cpu_composite/q14_stages.rs`）が番号で持つのは 8 と 17〜23 だけで、24〜33 は分岐に無い（来れば panic）。新しいモードを足す項目は、CPU 参照と JS 参照（`tests/e2e/cpu-composite.js`）の置き場を項目ごとに確かめ、無ければ作る。モード 6 は v3 に分岐が無く、0 を書く入口として使う（`tile_renderer/targets.rs` の `zero`）ので、新規に使わない。0〜16 は v1・v2 の `effects.wgsl` の番号で、v3 では使わない。34 以上は未使用。

| 番号 | 段 | 項目 |
|---|---|---|
| 34 | ハイパスの結合 | FX-4 |
| 35 | シャープの結合 | FX-1 |
| 36 | 移動ぼかし | FX-2 |
| 37 | ノイズ | FX-3 |
| 38 | 距離場の初期化 | FX-5（FX-6 も使う） |
| 39 | 距離場の1歩（JFA） | FX-5（FX-6 も使う） |
| 40 | 距離から被覆 | FX-5（FX-6 も使う） |
| 41 | 色つき平面 | FX-5（FX-6 も使う） |
| 42 | 勾配の平面 | FX-9（`gradient-q14.wgsl`（新規）を VEC-1 と共有） |
| 43 | 置き換え | FX-8 |

- 番号は段の働きに固定する。PR の順が変わっても番号は変えない。
- render v4 は v3 と同じ番号空間を使う（01 の RV4-1）。v4 のシェーダーは別ファイルで、同じ番号に同じ働きの Q30 版を置く。新しい番号は要らない。
- 番号を使わないもの：VEC-1 の着色（crop の dispatch `vector-gradient-q14.wgsl`（新規））、TXT-10 の `glyph-q30.wgsl`（新規。`glyph-q14.wgsl` と同じくモードの分岐を持たない）、IMG-5 の `image-edit-q14.wgsl`（新規。別のシェーダー）、STO-10 の目印（`upload` で上げるタイル）。
- FX-9 の結合は既存のモード 27 を使う。FX-6 の被覆は既存の 19（`outside`）と 27（`inside`）を使う。

### 文書の形式（`DocumentMeta::format_version`、今は 5）

| 回 | 値 | 項目 | 備考 |
|---|---|---|---|
| 6 | 6 | TXT-4、TXT-5、TXT-6、TXT-7（06） | TXT-4、6、7 は段階1で先に 6 を取る。TXT-5 は段階2で、最初のリリースの前なら同じ 6 に意味を足す（番号は上げない）。リリース後なら次の空き番号を取る。全文書で最初に上げる回。最初の PR が基盤（`FORMAT_VERSION`、`SUPPORTED_FORMATS`、manifest の `document_format`、読み込み、`gc.rs`、`merge.rs`）を入れる（新規） |
| 7 | 7 | VEC-1、VEC-2（07） | VEC-2 は段階1で先に入りうる。そのとき VEC-2 が 7 を取り、VEC-1 は同じ回 |
| 8 | 8 | FX-1、FX-2、FX-3、FX-4、FX-5、FX-6、FX-9（02） | FX-7 は形式に触れない |
| 9 | 9 | TXT-1、TXT-2（06） | |
| 10 | 10 | TXT-8（06） | |
| 11 | 11 | STO-9（09） | manifest の `format` は上げない（下） |
| 12 | 12 | TXT-3、TXT-9（06） | TXT-3a は形式に触れない。TXT-9 は `palette` を足すとき |
| 13 | 13 | RV4-1、COL-2、COL-4（01）、TXT-10（06） | `render_version` 4、`Rgba16`、`blend_space`（新規）、`text_rendering`（新規） |
| 14 | 14 | COL-3（01） | |
| 15 | 15 | COL-6（01） | `LayerProps.spot` |
| 16 | 16 | FX-8（02） | 段階4。回 8 と分ける |

上げない項目：COL-1（既定。既存の enum 値を使い、今の読み手は `INVALID_ARGUMENT` で拒否する。見直すなら 13）、COL-5、FX-7、全 BR（BR-2、BR-3 の案 B を選ぶときだけ、そのとき作業中の回に入れる）、全 SEL、全 IMG、TXT-11、VEC-3、VEC-4、全 VER と GIT、STO-9 以外の STO、全 RUN と PERF、COLLAB-1。

### manifest の形式（`format`、今は 2＝完全、3＝薄い）

| 値 | 項目 | 意味 |
|---|---|---|
| 4 | VER-4（08。最初に入れる）、VER-6、GIT-1 | stash、リモート、Git の先頭のどれかを持つ manifest。薄いかどうかは `external_history` の有無で決める（08 の案 B、既定） |

- 新しいフィールドがすべて空なら、今の番号（2 または 3）のまま書き、バイト列も今と同じ。
- 08 の案 A（完全と薄いに別の番号）を採るなら 4（完全）と 5（薄い）。
- VER-5 の出力は、stash かリモートが残れば 4、残らなければ 2。新しい番号を作らない。
- STO-5（プレビュー）は manifest の外のエントリなので上げない。STO-9 は `tables/` を足すが上げない。今の読み手は `document_format` の照合（`decode_entries_impl`）で 11 を先に拒否するので、`tables/` を黙って無視して開くことは無い。
- 今の試験（`crates/alto-store/src/lib.rs` の、薄い manifest の組み合わせを拒否する試験）は format 4 を未知の値として使っている。VER-4 で未知の値を別の番号（例：99）に変える。

### `PROTOCOL`（`packages/alto-sdk/src/protocol.ts`、今は 13）

破壊的な通信の変更では番号を上げる。同じ段階の項目は可能な限りまとめる。

| 値 | 回 | 項目 |
|---|---|---|
| 9 | 段階1 | BR-6（`init` の `undoSteps`）、STO-3（`exportImage` の選択肢）、VER-4（`historyCommand` の stash）、PERF-5（`recoveryStages`（新規）） |
| 10 | 段階2 | BR-1（`beginBrush` の角度と丸み） |
| 11 | 段階2 | BR-4、BR-5（`beginBrush` の `dynamics`） |
| 12 | 段階2 | BR-2（`beginBrush` の画像先端） |
| 13 | 段階2 | BR-3（`beginBrush` のテクスチャ）、IMG-1（`layerBake`（新規））、TXT-8（`fontAxes`（新規））、STO-4、STO-5、STO-9、STO-10、VER-5（`trimmedPackage`（新規）） |
| 14 | 段階3 | COL-1（`createDocument`）、COL-5（`import` の `profile`）、STO-1、GIT-2、STO-6（`cache` を足すときだけ）、RUN-1（`init` に版を渡すときだけ）、RUN-2（`unstable` に診断を足すときだけ） |
| 15 | 段階4の v4 | RV4-1 の段8（`renderVersion: 4` の公開）、COL-2、COL-4、COL-3、COL-6、TXT-10（作成の引数に属性を足すとき） |
| 16 | 段階4のほか | IMG-6（`imageWarp`（新規））、VER-6、STO-11、RUN-5（`exportBitmap`（新規））、COLLAB-1、IMG-5（採用時）、VEC-4（`vectorPreview` を足すとき）、FX-8（新しい要求が要るとき） |

上げない項目：IMG-3（段階1で IMG-1 より先に入り、結果に `missing` を足すだけ）、SEL-1〜3、TXT-1〜7、TXT-9、TXT-11、VEC-1〜3、FX-1〜7、FX-9、VER-1〜3、GIT-1、GIT-3（別パッケージ）、STO-2、STO-7、STO-8、STO-12、RUN-3、RUN-4、PERF-1〜4、PERF-6、PERF-7。13 の QA-5 で Worker の無言の終了を直す場合は、その時期の回に入れる。

## 効果と描画の追加手順

02、03、06、07 の描画タスクは共通してこの経路を通る（02 の「追加の経路」が詳細）。

| 段 | 場所 |
|---|---|
| 文書の型と検証 | `crates/alto-doc/src/effect.rs` の `Effect`、`effect_from_map`、`validate`。ノード当たりの個数は `crates/alto-doc/src/node.rs` |
| 描画用の型 | `crates/alto-engine/src/selection.rs` の `render_effect`、`effect_hashes`。`crates/alto-scene/src/lib.rs` の `REffectKind` |
| v3 の採否 | `crates/alto-gpu/src/renderer/geometry.rs` の `v3_gap`、`tests/e2e/cpu-composite.js` の `isIntegerEffect`（足し忘れると `v3-unsupported`） |
| GPU の段 | `crates/alto-gpu/src/renderer/tile_renderer/stages.rs`（`stage`、`stack`、`input_roi_v3`）、`graph.rs`（調整レイヤー）、`params.rs`（`q14_params`） |
| シェーダー | `crates/alto-gpu/shaders/effects-q14.wgsl` の `main`。連結は `integer_effects_shader`（`crates/alto-gpu/src/lib.rs`）。番号は上の「シェーダーのモード番号」 |
| float の禁止 | `crates/alto-gpu/src/renderer/tile_renderer/pool_tests.rs` の `v3_stages_call_no_f32_coordinate_helper` |
| CPU 参照 | `crates/alto-gpu/src/cpu_composite/`（`v3_tests.rs` の流儀で GPU と比べる） |
| JS 参照 | `tests/e2e/cpu-composite.js` の `isIntegerEffect` と評価部（BigInt） |
| SDK の型 | `packages/alto-sdk/src/types.ts` の `Effect`、公開 API は `packages/alto-sdk/api/alto-engine.api.md` |
| キャンバス編集 | `crates/alto-engine/src/canvas.rs` の `scale_effects` |
| ハッシュ固定 | `tests/e2e/parity-tests.js` に症例、`tests/parity/v3-browser.txt` に行を追記 |

## 文書をまたぐ重なり

2文書以上が同じファイル・型・関数を変える箇所と、決めた順序または持ち主。決めたことは両方の文書の「依存」にも書いた。

| 対象 | 変える項目 | 決めたこと |
|---|---|---|
| `Gradient`（`crates/alto-core/src/gradient.rs`（新規））、`gradient-q14.wgsl`（新規）、停止点の格納 | VEC-1（07）、FX-9（02） | 持ち主は VEC-1。VEC-1 → FX-9。FX-9 は 02 の案 A（07 の型を使う。停止点は非減少、`from`／`to`＋`transform`）。画像編集の既存の勾配（`start`／`end`、重複を拒否）は変えない |
| `effect_hashes` → `Document::object_hashes`（新規、alto-doc） | GIT-1（08）、FX-8（02） | GIT-1（段階3）→ FX-8（段階4）。GIT-1 が `node_hashes` と `effect_hashes` を移し、FX-8 が `DisplacementMap` を足す（新規） |
| `tile_maps_mut`（`crates/alto-doc/src/document.rs`） | STO-9（09）、FX-8（02） | STO-9 → FX-8。FX-8 が足す（新規） |
| `bake_ops`／`bake_text` の `Result` 化、`MAX_BAKE_GLYPHS`（新規）、`MAX_BAKE_BYTES`（新規） | 06 の共通の設計、VER-3（08）、COLLAB-1（12）、IMG-3（05） | 持ち主は 06。回 6（TXT-4〜TXT-7）の最初の PR で入れる。VER-3 と COLLAB-1 はその後。IMG-3 は `bake_ops` を変えないので順序の制約は無い |
| `ExportImageOptions`（`packages/alto-sdk/src/document.ts`）、`crates/alto-store/src/tiff.rs`（新規） | STO-3、STO-4（09）、COL-1、COL-2、COL-3、COL-6（01） | 持ち主は STO-3（段階1で先）。01 の項目は後から広げる。`tiff.rs` は「Rust はヘッダー、画素は JS」なので、COL-6 は CMYK の画素が WASM を通る作業量を最初に見積もり直す |
| `packages/alto-sdk/src/png.ts` | STO-4（09）、COL-1、COL-2、COL-5（01） | STO-4（段階2）→ 01 の項目 |
| `png` crate の直接依存 | VEC-3（07）、COL-5、COL-2（01） | VEC-3（段階2）が足し、01 は共有する。ZIP の展開器の決定（STO-11 の圧縮レイヤー）も同じ時に記録する |
| `tile_renderer/stages.rs`、`graph.rs` | FX-1〜6、FX-9、FX-8（02）、RV4-1（01）、PERF-4（10） | 段階2の効果 → PERF-4（実装するなら）→ RV4-1、FX-8。PERF-4 が再帰を明示タスクに直し、後の項目はその形で段を足す |
| `brush.wgsl`、`GpuDab.extra`、binding 6/7 | BR-1〜BR-3（03）、COL-2（01） | BR-1〜BR-3（段階2）→ COL-2。COL-2 が 16bit 版を足す |
| `SelectionSpec`（`document.ts`） | SEL-1 → SEL-2 → SEL-3（04） | この順 |
| `crates/alto-gpu/src/renderer/upload.rs`、`TileStore` | STO-10、STO-1（09） | STO-10（段階2）→ STO-1 |
| `ObjectTarget`（`crates/alto-engine/src/versions/lazy.rs`） | VER-1（08）、STO-1（09） | VER-1（段階2）→ STO-1 |
| `crates/alto-engine/src/versions/gc.rs` の format の検査 | 回 6 の PR（`document_format`）、VER-4（`format`）、VER-5、STO-1、STO-9 | 同じ行を別の条件で変える。どちらが先でもよい。後の PR が並べて書く |
| `VersionRefs`、manifest の照合（`crates/alto-store/src/lib.rs`） | VER-4、VER-5、VER-6、GIT-1（08） | VER-4 が `local_roots` と format 4 を入れ、他は使う（新規） |
| `crates/alto-engine/src/versions/branches.rs`、`versions.rs`、`merge.rs` | VER-1〜6（08）、COLLAB-1（12） | 08 が先。COLLAB-1 が `finish_merge`、`checkout`、`restore_version`、`stash_apply` に `collab-active` の拒否を足し、VER-3 の `merge_text` を `.ok()` で呼ぶ（新規） |
| `ContentDetail`（`crates/alto-doc/src/version.rs`、`crates/alto-git/src/lib.rs`） | 06（テキストのフラグ）、VEC-1／VEC-2（07、`style`） | 順序の制約なし。VEC-1 と VEC-2 は先に入る方が `style` を足す |
| `DocumentMeta` の新しい欄 | COL-4（`blend_space`）、TXT-10（`text_rendering`）、RV4-1 | 同じ回 13（新規） |
| `crates/alto-wasm/src/lib.rs` の `FRAME_FIELDS` | RUN-4、PERF-1（10） | 末尾に足し、JS は名前で読む。順序の制約なし |
| `packages/alto-sdk/src/worker.ts`、`protocol.ts`、`errors.ts` | `PROTOCOL` を上げる全項目 | 持ち主なし。同じ回の PR は直列にマージし、後の PR が取り込む |
| `ConflictReason`／`ErrorDetails.reason`、`WarningCode` | 08、09（`source-changed`、`document-changed`、`document-damaged`、`RECOVERED_STROKE`）、12（`collab-*`） | 値を足すだけ。名前が重ならないことを各 PR で確かめる（新規） |
| `crates/alto-engine/src/canvas.rs` | 02（`scale_effects`）、05（IMG-2、IMG-3） | 順序の制約なし |
| QA-5 の表と `stroke-open` の期待（`tests/e2e/crash-tests.js`） | STO-7（09）、13 | STO-7 の PR が 13 の表と `ENGINE-HARDENING-RESULTS.md` の D-1 を直す |
| `Editor` の挿入（`documents.insert`） | BR-6（03、`install_editor`（新規））、08 | BR-6 → 08 の項目は `install_editor` を使う |

## 実装の順序

依存があるものだけを示す。

1. QA-1 の残り（`folder`、`decode` の target、薄い `.alto` の seed）を先に入れる。以後の形式変更を読み込みファザーが検査する。QA-5 は実装済み。
2. 段階1の項目：BR-6（実装済み）、SEL-1 → SEL-2 → SEL-3、IMG-3、TXT-4、TXT-6、TXT-7（回 6。最初の PR が形式番号の基盤と焼き込みの `Result` 化を入れる）、VEC-2、STO-3、STO-8、PERF-5。VER-4 は manifest の形式の基盤として、VER-6、GIT-1 より前に入れる。
3. FX-4 → FX-1。FX-4 が共通基盤（`smoothing_radius_q8`、`effect_growth_q8`、`RoiQ::grow`、評価のメモ）を作り、`Context::stack` のぼかしの枝が選択つきで前段を2回評価する既存の問題も直す。（新規）
4. FX-5 → FX-6（距離場を FX-5 で作る）。FX-5 → IMG-1（`effect_output_bounds`）。VEC-1 → FX-9（`Gradient` の持ち主は VEC-1）。FX-2、FX-3、FX-7 は FX-4 の後なら順不同。（新規）
5. BR-1 → BR-2 → BR-3。BR-4 は BR-1 の描画を使い、BR-5 と共通の設定を使うため、BR-1 の後に BR-4 と BR-5 を進める。
6. TXT-5 → TXT-1 → TXT-2 → TXT-3（TXT-3a → TXT-3b）。ルビは行送りの計算を変えるため、段落属性の後に入れる。TXT-5 → VER-3（`remap_range`）。（新規）
7. STO-10、STO-5、VER-1 → STO-1 → IMG-4。STO-1 は STO-5 の `ZipIndex`（新規）を使う。STO-9 は STO-1 の前提ではない（同じ `lazy.rs`、`gc.rs` を変えるので、先に入れると衝突が減る）。IMG-4 は PERF-1、PERF-3 の後に測り直してから採否を決める。
8. PERF-5 → PERF-2 → PERF-1 → PERF-4。PERF-3 は PERF-5 の内訳で律速の項目を選ぶ。段階2の効果 → PERF-4 → RV4-1。RUN-2 → RUN-1。
9. COL-1 → COL-5 → RV4-1 → COL-2 → COL-4 → TXT-10 → COL-3 → COL-6。
10. VER-4 → VER-6 → GIT-1 → GIT-3。GIT-2 は VER-1〜VER-4 の後。
11. 残りの段階4（FX-8、IMG-5、IMG-6、VEC-4、STO-11、STO-12、RUN-3、RUN-5、COLLAB-1）。互いの依存は薄い。FX-8 は GIT-1、STO-9 の後。VEC-4 は PERF-1〜3、RUN-4 の後。STO-11 は IMG-1〜3 の後。RUN-5 は RUN-3、RUN-4 の後。COLLAB-1 は VER-3、VER-6、回 6 の後。

## 測定で実装の形が変わる項目

次の項目は実装の対象だが、測定の結果で範囲が変わる。各文書の判定条件に従い、結果を記録してから進める。数値は 7bdbe92〜bdf26a9 の記録で、各項目の最初の手順で測り直す（10）。

| ID | 先に測るもの | 結果の使い方 |
|---|---|---|
| RV4-1、COL-2 | Q30 の試作で、4K・20層の B1、B2、B6 を Metal で3回。B6 は GPU の計上値、WASM と復号プールの和、JS heap を分け、予算 2048 で測る。16bit はアトラスと表示タイルを `rgba16uint` にした試作で測る | 16bit の GPU の計上値の最大、または WASM と復号プールの和が 1 GiB を超えるなら、16bit と F16 は辺長 8192 まで（`MAX_DEEP_EDGE`（新規））。記録は `RV4-Q30-MEASURE.md`（新規） |
| COL-4 | 16bit の全 65536 値の TRC の往復 | 差が 1 を超えるなら表を 16385 点に増やす |
| FX-6 | width 64、4K の JFA の段の時間と `Budget` が出る最小の文書（Metal、SwiftShader） | B1 の p95 を超えるか通常の文書で `Budget` が出るなら、`outside` だけを先に出す |
| SEL-2 | `radius = 64` の `grow` の Worker の時間（SwiftShader と Metal） | `MAX_MODIFY_WORK`（新規）を他の要求を待たせない値に調整する。待たせすぎるなら、八角形の分解へ変える案を結果とともに利用者に示す（形が変わるので先に決めない） |
| BR-2 | 手書きの双線形の先端と `cpuReference` の差 | ±2 を超えるなら最近傍へ落とす |
| TXT-7 | `f64::tan` のネイティブと WASM の一致（`text-v3-skew`（新規）） | バイトが違えば、角度を 0.25° 刻みに丸め、`tan` を整数表に置き換える |
| IMG-4 | `stress-64mpx` の fit コールド表示（377〜380 ms）の内訳を、PERF-1、PERF-3 の後に測り直す | 250 ms を超え、超過の過半がレベル 0 の合成にあるときだけ採る |
| IMG-5 | 4096² の `resize`（4フィルタ）、`gradient` の内訳（native と WASM） | 符号化（`stage_tile`）の比率が 50% 未満の操作だけを GPU 化。`fill` は外す |
| VEC-4 | 密集ベクターの描画を、整数経路（PERF-1〜3 の後）と vello で比べる | コールドと warm の両方で2倍以上速いときだけ公開の選択肢。それ以外は `unstable` だけ |
| STO-1 | 予算 256 MiB、512 MiB、1 GiB で B1〜B7 と40分の長時間試験 | B1 の p95 の悪化が 20% 未満の最小を既定（暫定 1 GiB）。全体の pin が収まらなければ範囲の pin |
| STO-6 | PERF-5 と STO-1 の後に、C4 をキャッシュあり・なしで各5回 | 全可視 GPU 完了の中央値が 20% 以上短くなるときだけ採る |
| STO-9 | 120分の薄い `.alto` のエントリの種類ごとの大きさ | スナップショットが版あたりの増分の半分以上のときだけ実装 |
| RUN-1 | RUN-2 の後に、開く（B5）、B7、perf 群の `open .alto`、`recover` | さらに 20% 以上短くなり、WASM が2つになる配布の費用と引き合うときだけ採る |
| RUN-2 | 操作ごとの計算（符号化・ハッシュ・リサンプル）の比率 | 計算が半分以上で、受け渡しの複製を含めても短くなる操作だけ |
| RUN-4 | busy の拒否の回数と、コピー提出から map 完了までの時間（B3、B6） | 待ちの割合または実操作での busy 拒否と、B3 または確定時間の目標を合わせて判定（10 の採用条件） |
| PERF-1 | fixture ごとの共有できる入力の比率と fit コールド表示 | 共有が2割以上で短くなるときだけ採る |
| PERF-2 | `commands_ms` の比率 | どれかの fixture で1割以上、かつノード探索に由来するときだけ |
| PERF-3 | `stress-64mpx` などの fit コールド表示の内訳 | 律速の順に PERF-1、PERF-2、PERF-4、PERF-7、STO-6 を選ぶ。250 ms は希望値で、速度の約束はしない |
| PERF-4 | 効果付き文書（`fx`）の1タイルの同期時間 | 2 ms を超え、B3 や B8 の p95 に効くときだけ採る |
| PERF-7 | 変化の少ないグループの再合成の比率 | 判定の行の半分以上で、同じ内容の再合成が多いときだけ |

## 決定事項

| 番号 | 問い | 決定 |
|---|---|---|
| D1 | 段階4（対象外だった項目）を実装するか | する。AI と UI を除く |
| D2 | render v4 を導入するか | する。v3 は 8bit 文書の定義として残す |
| D3 | 共同編集と AI を本体の範囲に入れるか | 共同編集は入れる。AI は除外 |
| D5 | ICC の実装方法 | CMYK が LUT を要するので、自前で行列、TRC、LUT を扱う（CLUT は整数の四面体補間） |

## 利用者の決定が要る点

どれも既定の案で設計を書いてある。決めるまでは既定で進めてよい。

| 番号 | 問い | 文書と項目 | 既定の案 |
|---|---|---|---|
| D4 | 公式対応ブラウザ | 13 の QA-2、QA-3 | Chrome と Edge を正式、Safari と Firefox は QA-2 の結果で判断。決まるまで README の対応表は変えない |
| D6 | Playground の UI-1〜3 と、取り込み API（AI-2）も除外でよいか | 11、12 | 方針の言葉どおり除外。どれも小さく、後から戻せる |
| D7 | P3 の書き出しの色の印 | 01 の COL-1 | PNG に cICP。Chrome が読まないと分かれば iCCP にし、COL-1 の書き出しを COL-5 の後へ回す |
| D8 | 色空間・深さ・描画版を変える変換 API を作るか | 01 の COL-1〜COL-3 | 作らない（書き出して新しい文書へ読み込む） |
| D9 | render v4 を 8bit・ガンマ・`text_rendering` なしの文書に許すか | 01 の RV4-1 | `renderVersion: 4` を明示したときだけ許す（新規） |
| D10 | 16bit 文書の画像の読み込みの上限 | 01 の COL-2 | 32M 画素。RV4-1 の測定で B6 が収まれば 64M 画素 |
| D11 | F16 文書の Normal、Multiply、Add 以外の合成モード | 01 の COL-3 | 各入力を [0, 1] に丸めてから v4 の式 |
| D12 | CMYK プロファイルを同梱するか | 01 の COL-6 | 同梱しない。試験は合成したプロファイル |
| D13 | ブラシの画像先端・テクスチャを文書に保存するか | 03 の BR-2、BR-3（09 の STO-7 の描き直しの範囲も決まる） | 保存しない。SDK が画素列を持ち、ストロークの開始時に渡す（形式に触れない） |
| D14 | `pressureSize`／`pressureOpacity` を消して `dynamics` にまとめるか | 03 の BR-5 | 消す（SDK の破壊的変更） |
| D15 | パターンの座標系 | 03 の BR-3 | レイヤー座標 |
| D16 | Undo のバイト数に上限を足すか | 03 の BR-6 | 足さない（測ってから） |
| D17 | ブラシの速度の正規化 | 03 の BR-5 | 4 px/ms（レイヤー画素） |
| D18 | `color`／`contiguous` の選択が空のとき `None` か `Some(空)` か | 04 の SEL-1 | `Some(空)` にそろえる。`invert` の現状は変えない |
| D19 | SDK 0.x の公開型の破壊的変更の版の上げ方 | 04 の SEL-1（`SelectionSpec`）、03 の BR-5 | `RELEASING.md` の方針に従って版を上げ、変更履歴に破壊的変更として書く |
| D20 | SVG の `<image>` の JPEG を Rust で復号するか（新しい依存） | 07 の VEC-3 | PNG のみで始め、JPEG は決定後 |
| D21 | 画素内の自動マージでマスクを指定できるようにするか | 08 の VER-2 | 入れない |
| D22 | 同じ位置への文字の挿入を競合にするか | 08 の VER-3 | 競合にする |
| D23 | 打ち切った履歴を薄い `.alto` と新しい置き場の組で出すか | 08 の VER-5 | 第一段階では作らない（完全な `.alto` か `.altoproj`） |
| D24 | rebase 後に Git 側を強制更新する選択肢（`--force-branch`） | 08 の GIT-2 | 足さない（`sync` は `CONFLICT`） |
| D25 | 保存ファイルに既定でプレビューを入れるか | 09 の STO-5 | `preview: true` のときだけ入れる |
| D26 | ネイティブ組み込みの形 | 10 の RUN-3 | `examples/native-viewer` の中だけでつなぎ、`alto-gpu` に `new_headless` を1つ公開する（案 A）（新規） |
| D27 | 外部 GPU への書き出しの API 名 | 10 の RUN-5 | `doc.exportBitmap`（新規） |
| D28 | 共同編集で共有するもの | 12 の COLLAB-1 | 作業文書だけ（版は各自） |
| D29 | 接続中のキャンバス寸法の変更 | 12 の COLLAB-1 | 断る |
| D30 | 順序係の認証と権限 | 12 の COLLAB-1 | 範囲外（参照実装は認証なし。組み込み側が用意する） |
| D31 | 共同編集のログの圧縮 | 12 の COLLAB-1 | 第一段階は無し（`COLLAB_MAX_LOG_TXS` で部屋を閉じる）（新規） |
| D32 | 他者の編集を commit の意図の一覧に入れるか | 12 の COLLAB-1 | 入れない |
| D33 | Playwright を開発依存に入れるか | 13 の QA-2 | 入れない（素の CDP のまま） |
| D34 | self-hosted の GPU ランナーを登録するか | 13 の QA-3 | 登録しない（`scripts/gpu-matrix.mjs`（新規）を手で回す） |
| D35 | TS のジャーナル読み取りが最初の不正な記録で後続を捨ててよいか | 13 の QA-1 | 今のまま（捨てる）。変異試験で記録数を固定する |

## 製品の前提が変わる点

- **外部送信。** README の「外部へ送信する処理はない」は SDK 本体について維持する。STO-12（Drive）、GIT-3（ブラウザ内 Git）、VER-6（同期）、COLLAB-1（共同編集）のネットワーク処理は別パッケージに置き、利用者の明示的な呼び出しでのみ動かす。
- **描画経路の二重化。** render v4 の導入後は v3 と v4 の両方を保守する。描画機能を足すたびに実装が6つになる。v4 の機能は順番を詰め、v3 と式を共有できる箇所は WGSL の共通ファイルにする。RV4-1 より前に v3 へ入る描画の項目（02、07）は、RV4-1 の段5〜7で Q30 版を作る。

## 計画外の候補

実装の計画に無い欠け。項目にはしていない。範囲に入れるかは別に決める。

- **PDF の書き出し。** 書き出しは PNG、WebP（STO-3 で JPEG、TIFF、STO-11 で PSD）だけで、印刷向けの PDF は無い。
- **SVG の書き出し。** VEC-3 は SVG の取り込みだけを広げる。ベクターとテキストを SVG に書き出す経路は無い。
- **OS のフォントの利用。** フォントは文書に埋め込むアセットだけで、ブラウザのシステムフォント（Local Font Access API）は使わない（`doc/MILESTONES.md` M7）。
- **Issue #13（P2P 接続時の `render_version` の交渉）。** 接続時に対応する `render_version` を交換し、知らない版を要る Op を拒否する。COLLAB-1 の `hello` は描画の版と `FORMAT_VERSION` の一致を要るので一部を満たすが、VER-6 の同期や将来の P2P に一般の規則は無い。（新規）
- **旧形式からの移行。** 最初のリリースまで作らない（共通規則3）。リリースの後に形式を上げる最初の PR で要る。
- **Worker の無言の終了の検出。** Worker が `close()` で終わると再起動されず、要求が返らない（13 の QA-5 で再現だけを試験にする）。直すには要求の無応答の監視の設計が別に要る。
- **ほかのレイヤースタイル。** 02 はドロップシャドウと境界線だけを足す。内側の影、光彩、ベベルは無い。
- **選択の上限の重複。** `crates/alto-engine/src/canvas.rs` にも private の `MAX_SELECTION_TILES` があり、`selection.rs` のものと重なる（04）。
- **SDK の `Color` の注釈。** `packages/alto-sdk/src/types.ts` の `Color` は「Linear RGBA」と書くが、v3 は文書の色空間の符号化値をそのまま使う（01）。
