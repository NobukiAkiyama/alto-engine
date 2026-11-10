# 画像の書き出しと画素取得の契約

作成日：2026-11-09。[エンジンの修正・検証計画](2026-11-09-engine-hardening-plan.md)の段階C。

## 経路

| API | 経路 | 返す画素 |
|---|---|---|
| `DocumentHandle.exportImage` | `document.ts` → Workerの `exportImage` → WASMの `readTiles`（GPUの表示タイルを読み戻す）→ 符号化 | キャンバスに表示される合成結果 |
| `image.pickComposite` | Worker → WASMの `pickComposite`（表示タイルの1画素） | 同じ合成結果の1画素 |
| `image.pickColor` | Worker → WASM → `Engine::pick_color`（CPUで層のタイルを読む） | 1つのラスター層の保存画素。合成結果ではない |

`exportImage` は表示タイルをそのまま使う。合成（17合成モード、不透明度、通過・分離グループ、マスク、クリップ、効果、テキスト、ベクター、変形）の正しさは、表示タイルとCPU参照（`crates/alto-gpu/src/cpu_composite/`、`tests/e2e/cpu-composite.js`）を比べるparity試験で確かめる。書き出しの契約は、表示タイルから画像ファイルまでの変換だけを対象にする。

## 入力

- `scale`：0より大きく1以下。それ以外（NaNを含む）は `INVALID_ARGUMENT`、`reason: "scale"`。
- `region`：文書の画素座標。省略時は文書全体。各座標がNaNまたは数値でなければ `INVALID_ARGUMENT`、`reason: "region"`。範囲は `x0`・`y0` を切り捨て、`x1`・`y1` を切り上げてから文書に収める。無限大は文書の端になる。収めた結果が空なら `INVALID_ARGUMENT`。
- 大きさ：読む画素が6,400万を超える場合と、作業メモリが512 MiBを超える場合は `INVALID_ARGUMENT`。部分的な画像は返さない。
- `type`：`image/png`（既定）または `image/webp`。

## 出力の画素

表示タイルはpremultipliedのsRGB 8bitである。書き出しは各画素を次の式でstraight alphaに戻す。

- `a = 0` のとき：`(0, 0, 0, 0)`
- それ以外：`round(c × 255 / a)`（0.5は切り上げ）。`a` はそのまま

`pickComposite` も同じ規則で戻すので、同じ画素では書き出しと同じ値を返す。

| 条件 | 保証 |
|---|---|
| PNG、`scale = 1` | デコード後の画素が上の式の結果と完全に一致する |
| PNG、`scale = 1/2^k`、範囲の始点がピラミッド段 `k` の画素に乗り、出力の寸法 `round(範囲 × scale)` が読んだ段の画素数と等しい | 段 `k` の表示タイルについて完全に一致する。範囲の終点が段の画素の途中にあるとき（例：幅99を1/2で書き出す）、最後の列・行はその段の画素をそのまま使うので、範囲外の文書画素を半分含む |
| PNG、それ以外の倍率 | ブラウザの `drawImage`（`imageSmoothingQuality = "high"`）で縮小する。画素値は保証しない。寸法は `round(範囲 × scale)`（最小1） |
| WebP | ブラウザが符号化する。寸法と形式だけを保証する |

完全一致の場合、PNGはSDKが自前で符号化する（`packages/alto-sdk/src/png.ts`：8bit RGBA、インターレースなし、sRGBチャンク付き）。2026-11-09までは常にcanvasの `convertToBlob` を使っていた。canvasは画素をpremultipliedで持つので、半透明の画素で色が最大1ずれていた（512×256の試験画像で3,584画素）。

## `pickColor`

- 返すのは指定したラスター層のタイルに保存されたバイトそのままで、alphaの変換をしない。その層の変形・不透明度・合成モード・上下の層は反映しない。
- 座標は層の画素座標で、小数は切り捨てる。非有限値とi32の範囲外は `INVALID_ARGUMENT`、層の範囲外は `NOT_FOUND`、ラスター以外の層は `INVALID_ARGUMENT`。
- 表示されている色が欲しい場合は `pickComposite` を使う。

## 試験

`tests/e2e/embed-tests.js`：

- 「exportImage PNG holds the display tiles exactly…」：300×160（2タイル）の文書を2つ書き出す。1つ目は半透明のグラデーションに17合成モードを不透明度0.8で重ねた層と、負の座標へずらした層。2つ目は同じグラデーションに、不透明度0.7の分離グループ（乗算の層と負の座標へずらした層）、ベクター、レベル補正の調整レイヤー。PNGを自前でデコードし、全画素を表示タイルと比べる。タイルの境界をまたぐ奇数の範囲、1/2倍、`pickComposite` の5点も比べる。文書を分けたのは、すべてを1つにするとM5の合成器を通る1フレームがSwiftShaderで3.5〜5秒かかり、描画の監視（約2秒）が `GPU_LOST` を出すため。分けると合成モードはタイルの合成器で描かれ、M5の2フレーム目は約0.4秒になる。
- 「exportImage refuses NaN region coordinates…」：NaNの範囲を拒否すること、無限大の範囲で文書全体になること、空の文書が透明になること。
- 既存の「exportImage writes the displayed pixels, scaled and cropped」：WebPの寸法と、不正な `scale` の拒否。

`packages/alto-sdk/test/png.test.js`：エンコーダがチャンクのCRC・ヘッダ・画素を正しく書くこと。

テキストとマスクは上の文書に入れていない。どちらも表示タイルの段階で合成が終わり、書き出しの変換は層の種類に依存しないためである。合成の正しさはparity試験で確かめている。
