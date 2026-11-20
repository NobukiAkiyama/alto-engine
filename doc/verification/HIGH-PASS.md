# ハイパス（FX-4）

レイヤーの効果スタックや調整レイヤーで `type: "high_pass"` を使う。半径は 0.5〜64 px、既定値は 8 px。選択範囲を持てる。効果は元のアルファを維持する。

ハイパスを使用した文書は形式8で保存する。形式5〜7の文書を引き続き読み込める。形式8のスナップショットとパッケージの往復をテストした。

## 画素と予算

- Rust と JavaScript の独立参照で、均一色、透明画素、半透明の勾配を検証した。Metal と SwiftShader の描画比較は各169件が通過。新規8行には縦横のタイル境界、選択つき効果、調整レイヤーを含み、既存85行のハッシュを維持した。
- 拡大時の選択つきハイパスと、変形つきのネストしたグループ・ラスターレイヤー・調整レイヤーを混在させた場合も、各効果の適用範囲をネイティブ描画で確認した。
- 選択つきぼかし32段について、入力レベル −6〜0 で同じ接頭辞とレベルを一度ずつ評価する計画を検証した。レベル −4 のネイティブ描画では、256 MiB と 512 MiB の GPU 予算で全画素が一致し、選択範囲外の画素も維持した。
- プールでは同じ寸法の空き枠を先に探す。新規割り当てが予算を超える場合は、ROI を収容できる空き枠を再利用する。使用中の枠は保持し、最後の読み取りをエンコードした後に解放する。

## 確認コマンド

```sh
sh scripts/check.sh native
pnpm --dir packages/alto-sdk build
pnpm --dir packages/alto-sdk api:check
node scripts/verify-browser.mjs --functional --suite=functional,parity
ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --functional --suite=parity
```
