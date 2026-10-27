# 密集ベクターの順序を保つ MRT 描画

2026-10-26。対象は先行する uniform arena / disjoint batching 実装。実装結果は [検証レポート](../../verification/DENSE-VECTOR-RESULTS-2026-10-26.md)に分ける。

## 問題

重なるパスは disjoint batching でまとめられない。1,000パスに塗りと線があれば、出力 clear 1回＋2,000個の coverage / paint で4,001 render pass が必要になる。先行測定は Apple M5 / Metal の warm 中央値221 ms。順序を入れ替えたり、半透明の三角形を直接描いて stroke の自己重なりを濃くしたりする方法は採用しない。

## 独立した coverage を8枚用意する

通常の R8 coverage に7組の 4x MSAA / resolve texture を追加し、一つの render pass に8組を attachment として渡す。各 Mesh バッチを別の attachment に描く。

geometry の shader は既存の頂点変換をそのまま使う。fragment は8個の出力へ1を返し、8種類の pipeline がそれぞれ1個の attachment だけで red の書き込みを許可する。他の attachment は write mask を空にして変更しない。blend は従来どおり None。自己重なりでも coverage の1を書くだけであり、alpha の足し算にはしない。

pass 終了時に各 R8 coverage を resolve し、一つの paint pass でバッチ0→7を元の順序で合成する。8回の draw は維持し、各 draw は対応する coverage を textureLoad する。次の8バッチの前に全 coverage を clear する。

```text
元の Mesh 順序: A塗り, A線, B塗り, B線, …
  coverage pass: 独立した8枚へ、その順序で geometry を描く
  paint pass:    対応する8枚を読み、元の順序で色を合成する
```

描画先の座標と大きさ、scissor、4x MSAA、R8 resolve、RGBA16F の blend 式を維持する。mask atlas への座標移動も、色の RGBA8 への丸めも行わない。paint は従来と同じ `a = coverage * color.a; rgba = (color.rgb * a, a)` を使う。

2,000バッチでは `1 + 2 × ceil(2000 / 8)`、501 pass が構造上の目標になる。clear / resolve する論理画素数は減らない。図形の準備・draw 数・pixel の処理を無くす設計でもない。pass 境界の削減が実機で有利か測って判断する。

## 選択と予算

- 8パス以上で、disjoint 計画のグループ数がパス数の80%以上なら密集経路を候補にする。離れた100パスは従来の5 pass を維持する。
- device の color attachment 数、attachment bytes / sample、sampled texture 数を確認する。条件不足では既存の経路に戻す。text / glyph と vector mask は最適化の対象を広げない。
- 追加の7組は必要時だけ作る。512²では論理的に `512 × 512 × 5 × 7 = 9,175,040 bytes` を追加する。driver 全体の実使用量を示す値ではない。
- 通常の uniform と、slot を持つ64-byte dense uniform は別の arena を使う。device の alignment を守り、既存と同じ256 slots / page、最大16 pages。両方の arena と追加 coverage を既存の GPU 予算に含める。
- coverage と必要な arena の両方が予算に収まることを先に確認する。入らなければ disjoint / pooled / legacy へ戻す。追加 submit や GPU 完了待ちで帳尻を合わせない。
- 外部の確保量が増え、以前の dense scratch が通常描画の予算を圧迫する場合は、その cache を退役させて通常描画を可能にする。

## 資源寿命

未提出の全描画に一意の uniform offset を渡す。ROI の変更に伴い、通常 coverage または dense coverage のいずれかが変わったら paint bind group を無効にする。古い texture / buffer は既存の Retired へ渡し、それを使う command の submit 後に解放する。

同じ encoder で ROI サイズを変える場合、複数ページにまたがる場合、待たずに連続 submit する場合、予算圧迫で cache を捨てる場合を専用に検査する。ウォーム反復で管理対象の量が増え続けないことも確認する。

## 採用判定

- Legacy / Pooled / Disjoint（先行実装）/ Dense（強制 MRT）/ Batched（自動選択）を同じ device / queue 上で比較する。
- RGBA16F 全バイトを比較する。色の異なる半透明、0 / 微小 alpha、塗り＋線、自己重なり、ROI / 変換、8の倍数でないバッチ数を含める。
- 実 GPU の独立5プロファイル以上、方式の順序を交互にして、3 warmup＋100フレーム。今回の採用測定は正逆3回ずつ、計6プロファイル。CPU encode と queue 完了を区別し、readback は計時外。全サンプル・失敗・実行 WASM hash を保存する。
- 密集1,000パスが先行 Disjoint の50%以下を目指す。疎な100パスは5 pass のまま、画素一致と継続的な速度悪化がないことを確認する。
- SwiftShader は正しさと障害経路の確認に使う。SDK の密集 vector layer を通して、編集、Undo/Redo、tile seam、mask / effect、保存・再読込、GPU 再作成も確認する。

8枚方式が実測で不利なら採用しない。これは Canvas / compositor の間欠的 GPU 喪失を修正する設計ではない。

## 参照

- [wgpu 30.0.1 ColorTargetState](https://docs.rs/wgpu/30.0.1/wgpu/struct.ColorTargetState.html)：write mask と attachment format の契約。
- [同版 RenderPassColorAttachment](https://docs.rs/wgpu/30.0.1/wgpu/struct.RenderPassColorAttachment.html)：MSAA の resolve target。
- [WebGPU correspondence](https://gpuweb.github.io/gpuweb/correspondence/)：MRT 数と attachment bytes / sample の実装上の上限。
- [先行実装の結果](../../verification/VECTOR-OPTIMIZATION-RESULTS-2026-10-26.md)：離れたパスの改善と、密集時に残った費用。
