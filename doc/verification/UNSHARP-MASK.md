# シャープ（FX-1）

レイヤー効果と調整レイヤーで `unsharp_mask` を使用できる。半径は 0〜64 px、強さは 0〜5、しきい値は 0〜255 の整数。既定値は半径 8 px、強さ 1、しきい値 0。

```ts
await doc.effects.set(layer, [
  { type: "unsharp_mask", radius: 5, amount: 2, threshold: 8 },
]);
```

ガウシアンぼかしとの差を元の色に加え、元のアルファを維持する。選択領域があれば、その範囲内で効果を適用する。半径・強さがゼロの場合は元の画素を保持する。しきい値以下のチャンネルも元の値を保持する。

アンシャープマスクを含む文書は形式8で保存する。

## 確認コマンド

```sh
sh scripts/check.sh native
sh scripts/build-m0.sh
pnpm --dir packages/alto-sdk build
pnpm --dir packages/alto-sdk api:check
node scripts/verify-browser.mjs --suite=parity
ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --suite=parity
node scripts/verify-browser.mjs --suite=embed '--embed-test=unsharp mask identity'
```
