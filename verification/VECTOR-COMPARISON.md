# M6：lyon / vello 比較


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

## 方法

2026-10-18、このMacの内蔵Chromium 155・WebGPUで実行しました。kurboで作った同一の型付きパス100本を10×10に配置し、512×512へ曲線の塗りと半透明の線を描画します。3フレームのウォームアップの後、20フレームを測定。編集を行わない同一Scene/メッシュを再使用します。

時刻はWorkerのperformance.now。CPUでのコマンド作成・submit・GPU queue完了を含み、GPU timestamp queryではありません。メッシュ作成とパイプラインの初回処理は「初回」に含め、別にwarm時を記録します。初回のブラウザー/ドライバーのキャッシュ状態は管理していないため、コールド起動の比較には使えません。

| 指標 | lyon 1.0.22 + Alto描画 | vello 0.11.0 |
|---|---:|---:|
| AA | 4x MSAA・R8カバレッジ | Area |
| レンダラー構築 | 0.80 ms | 2.10 ms |
| 初回描画＋GPU完了 | 48.00 ms | 7.90 ms |
| warm 中央値 | 25.90 ms | 0.80 ms |
| warm p95 | 30.20 ms | 1.00 ms |

velloのSceneエンコードは0.40ms。lyonのGPUメッシュ/カバレッジ保持量は1,521,920bytes、100エントリー・初回100 miss・以後2200 hitでした。vello内部のGPU確保量は測っていないため、メモリの優劣は比較していません。

## 画素の比較

Altoはpremultiplied RGBA、velloの出力はstraight alphaです。velloの読み戻し結果をpremultipliedへ変換してから比較しています。

- 境界を避けた塗り内部の11,700画素：チャンネル差3/255を超えた画素は0。
- 全画素のチャンネル絶対差平均：1.383/255、最大：71/255。
- 非ゼロalphaの画素数：lyon 104,100、vello 106,400。

AA方式が異なるため、境界のサンプル数・カバレッジは一致しません。この比較を画素完全一致のテストにはしていません。M6本体の輪郭検証には、別途、解析式がある二次曲線・16段階の表示レベル・タイル境界でのテストを使用しています。

## 採用判断

**この100パスのwarm描画ではvelloが速い**結果でした。Altoのlyon描画は塗り/線ごとにR8カバレッジと色のパスを分けており、パス数が増える場面にはGPUパス数の削減余地があります。lyonライブラリ単体とvelloの汎用的な速度比較ではなく、今回実装したAlto描画とvelloの比較です。

M6本体は、既存のタイルROI・RGBA16F中間値・M5のマスクとぼかしへ統合したlyon経路を採用します。velloは別WASMで実行する比較対象として同梱しています。単一の測定から本体の合成経路を変更する判断はしていません。密なベクター文書向けの次の改善候補は、カバレッジ描画のバッチ化とvelloの本体アダプターです。本体のバックエンド切替は現在公開していません。

`alto-vector::VectorRasterizer` の関連型付き `prepare` を使い、lyonはMesh、比較用velloはSceneへ同じパスを準備します。保存データと編集Opはバックエンドから独立しています。

## 再現

```sh
sh scripts/run.sh
# http://localhost:8765/examples/vector-comparison/ で「比較を実行」
```

比較用WASMの再ビルドは `sh scripts/build-vector-comparison.sh`。本体WASMは2,532,853 bytes / gzip 599,840 bytes、比較用WASMは4,223,713 bytes / gzip 984,224 bytesです。比較版には本体のRuntimeもリンクされているため、この差はvelloライブラリ単体のサイズではありません。

生JSON（`m6-vector-comparison.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`） / [比較版Clippy](m6-comparison-clippy.log)。上流：[lyonの塗り/線テッセレーション](https://docs.rs/lyon_tessellation/1.0.22/lyon_tessellation/)、[kurboのパス変換](https://docs.rs/kurbo/0.13.1/kurbo/struct.BezPath.html)、[velloのGPU描画](https://docs.rs/vello/0.11.0/vello/)。
