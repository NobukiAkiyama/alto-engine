# 未実装機能の設計と実装計画

作成日：2026-11-04。基点は main `b2036ce`（PR #75 マージ後）。

> **For agentic workers:** 各設計書の「実装タスク」を、superpowers:subagent-driven-development または superpowers:executing-plans で1タスクずつ実行する。

**Goal:** 文書に記載された未実装項目のすべてに、実装に着手できる設計と作業手順を用意する。

**Architecture:** 領域ごとに1文書。各文書は設計（データモデル、API、アルゴリズム、上限、エラー）と実装タスク（ファイル、インターフェース、試験、完了条件）を持つ。本書は全項目の対応表、共通規則、順序、利用者の決定が要る点をまとめる。

**Tech Stack:** Rust / wgpu / WGSL / WASM、TypeScript SDK、Chrome（SwiftShader、Metal）。

状態：設計のみ。本書の作成時点で、ここに挙げた機能のコードは1行も追加していない。工数は相対値で、実測ではない。

**方針（2026-11-04、利用者の決定）：AI と UI 以外をすべて実装する。** AI 機能（SEL-4、AI-2、AI-3）と UI 項目（UI-1〜5）は除外し、設計だけを記録として残す。共同編集は AI 機能ではないので実装の対象とする（COLLAB-1。旧 AI-1）。

## 文書

| 文書 | 領域 |
|---|---|
| [01-color.md](01-color.md) | 色空間、ビット深度、リニア合成、ICC、CMYK |
| [02-effects.md](02-effects.md) | 追加効果、自動補正、複数入力、非破壊グラデーション |
| [03-brush.md](03-brush.md) | ブラシ先端、傾き、筆圧カーブ |
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
| STO-2 | 文書別ジャーナル | M | 2 | なし |
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
| QA-1 | ファジング | M | 1 | なし |
| QA-2 | Safari、Firefox、Edge | L | 1 | なし |
| QA-3 | 別GPUの実機 | M | 1 | なし |
| QA-4 | 長時間運用 | M | 2 | なし |
| QA-5 | タブ強制終了 | S | 1 | なし |

「旧形式からの移行」は実装しない。リリース済みの形式がなく、移行元がない。最初のリリース以降の形式変更で移行を実装する（共通規則3）。

## 共通規則

全タスクの要件に暗黙に含まれる。

1. **既存の画素を変えない。** `tests/parity/cpu-reference.txt`、`tests/parity/v3-browser.txt`、`tests/parity/text-bake.txt` の既存行は変更しない。既存文書の画素が変わる変更は render v4 として本書の該当節を更新してから行う。
2. **描画の3実装。** render v3 の画素を出す機能は、Rust の CPU 参照（`crates/alto-gpu/src/cpu_composite.rs`、試験専用）、JS の参照（`tests/e2e/cpu-composite.js`）、WGSL（`crates/alto-gpu/shaders/effects-q14.wgsl` など）を揃え、Metal、lavapipe、SwiftShader で全バイト一致させる。v3 のシェーダーに浮動小数の演算を入れない。文書の f32 / f64 は、ホスト側で整数へ量子化してからシェーダーへ渡す。
3. **形式。** 文書の意味を増やす変更は `DocumentMeta::format_version` を上げ、`deny_unknown_fields` の型へ追加する。最初のリリースまでは移行を作らない。形式番号の重複を避けるため、同じ時期のPRは1回の番号上げにまとめる。Worker との要求を増やす変更は `packages/alto-sdk/src/protocol.ts` の `PROTOCOL` を上げ、README と RELEASING を同じPRで更新する。
4. **編集の不変条件。** 編集は検証してから適用し、失敗は状態を変えない。可逆 Op の1トランザクションで1回の Undo に対応する。ジャーナル、保存、再オープン、コミット、差分、マージ、GPU喪失後の再構築を通す。
5. **上限。** 入力の上限を定数で定め、超えたら原子的に拒否する。作業量に上限のない処理を作らない。
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

## 効果と描画の追加手順

02、03、06、07 の描画タスクは共通してこの経路を通る。

| 段 | 場所 |
|---|---|
| 文書の型と検証 | `crates/alto-doc/src/lib.rs` の `Effect`、`effect_from_map`、`validate_effect` |
| シーン | `crates/alto-scene/src/lib.rs` の `REffectKind` |
| GPU の段 | `crates/alto-gpu/src/renderer/m5.rs` の `stage`、`input_roi_v3` |
| シェーダー | `crates/alto-gpu/shaders/effects-q14.wgsl`。使用中のモードは 8、17〜33。新規は 34 から順に割り当てる |
| CPU 参照 | `crates/alto-gpu/src/cpu_composite.rs` |
| JS 参照 | `tests/e2e/cpu-composite.js` の `isIntegerEffect` と評価部 |
| SDK の型 | `packages/alto-sdk/src/types.ts` の `Effect`、公開 API は `packages/alto-sdk/api` |
| ハッシュ固定 | `tests/parity/` に行を追加 |

## 実装の順序

依存があるものだけを示す。

1. QA-1、QA-5 を先に入れる。以後の形式変更を読み込みファザーが検査する。
2. 段階1の独立項目（BR-4、BR-5、BR-6、SEL-1〜3、IMG-3、TXT-4、TXT-6、TXT-7、VEC-2、VER-4、STO-3、STO-8、PERF-5）。
3. FX-4（ハイパス）→ FX-1（シャープ）。両方がぼかし段の再利用で同じ結合段を使う。
4. FX-5（シャドウ）と FX-6（境界線）。ノード範囲外へ広がる効果の共通基盤を FX-5 で作る。FX-9、VEC-1 は同じ勾配評価を共有する。
5. BR-1 → BR-2 → BR-3。
6. TXT-5 → TXT-1 → TXT-2 → TXT-3。ルビは行送りの計算を変えるため、段落属性の後に入れる。
7. STO-9 → STO-1 → IMG-4。部分読み込みはタイルの取得経路を変えるため、ミップの鍵を決める前に入れる。
8. PERF-2 → PERF-1 → PERF-4。
9. COL-1 → COL-5 → RV4-1 → COL-2 → COL-4 → TXT-10 → COL-3 → COL-6。
10. 残りの段階4（FX-8、IMG-5、IMG-6、VEC-4、VER-6 → GIT-3、STO-11、STO-12、RUN-3、RUN-5、COLLAB-1）。互いの依存は薄い。COLLAB-1 は VER-3 と VER-6 の後に置く。

## 測定で実装の形が変わる項目

次の項目は実装の対象だが、測定の結果で範囲が変わる。各文書の判定条件に従い、結果を記録してから進める。

| ID | 先に測るもの | 結果の使い方 |
|---|---|---|
| RV4-1、COL-2 | Q30 の Normal 合成の B1、B2、B6 | B6 が 1 GiB を超えるなら辺長 8192 までに制限 |
| IMG-5 | 4K の `resize`、`gradient`、`fill` の内訳 | 符号化の比率が 50% 未満の操作だけを GPU 化 |
| VEC-4 | 密集ベクターの描画時間 | 整数経路より 2 倍以上速い場合だけ既定の候補にする |
| PERF-1、PERF-3、PERF-7 | 現在の B2、C1、効果付きズームの内訳 | 律速の項目だけを実装 |
| RUN-1、RUN-2 | 保存とフィルタの内訳（WASM） | B5、B7 の短縮が 20% 未満なら配布の変更を見送る |

## 決定事項

| 番号 | 問い | 決定 |
|---|---|---|
| D1 | 段階4（対象外だった項目）を実装するか | する。AI と UI を除く |
| D2 | render v4 を導入するか | する。v3 は 8bit 文書の定義として残す |
| D3 | 共同編集と AI を本体の範囲に入れるか | 共同編集は入れる。AI は除外 |
| D5 | ICC の実装方法 | CMYK が LUT を要するので、自前で行列、TRC、LUT を扱う（CLUT は整数の四面体補間） |

## 利用者の決定が要る点

| 番号 | 問い | 既定の案 |
|---|---|---|
| D4 | 公式対応ブラウザ | Chrome と Edge を正式、Safari と Firefox は QA-2 の結果で判断 |
| D6 | Playground の UI-1〜3 と、取り込み API（AI-2）も除外でよいか | 方針の言葉どおり除外。どれも小さく、後から戻せる |

## 製品の前提が変わる点

- **外部送信。** README の「外部へ送信する処理はない」は SDK 本体について維持する。STO-12（Drive）、GIT-3（ブラウザ内 Git）、VER-6（同期）、COLLAB-1（共同編集）のネットワーク処理は別パッケージに置き、利用者の明示的な呼び出しでのみ動かす。
- **描画経路の二重化。** render v4 の導入後は v3 と v4 の両方を保守する。描画機能を足すたびに実装が6つになる。v4 の機能は順番を詰め、v3 と式を共有できる箇所は WGSL の共通ファイルにする。
