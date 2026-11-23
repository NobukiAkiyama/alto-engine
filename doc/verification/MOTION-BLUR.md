# 移動ぼかし（FX-2）

`motion_blur` は角度に沿った1次元の箱平均です。角度の範囲は −180〜180 度、距離は 0〜128 px。既定値は角度0度、距離4 pxです。選択範囲と調整レイヤーに対応し、出力範囲外は透明としてサンプルします。

```ts
await doc.effects.set(layer, [
  { type: "motion_blur", angle: 45, distance: 12 },
]);
```

距離の半分を Q8 半径に量子化してサンプル間隔を求めます。各 premultiplied Q14 チャンネルを同じタップ数で平均するため、アルファも同じ規則に従います。Canvas を四分回転すると角度が変わり、等倍率リサイズでは距離が倍率に従います。不均一リサイズは拒否します。

FX-1／FX-4 と同じく、効果を含む文書は形式8で保存します。

## 確認コマンド

```sh
sh scripts/check.sh native
pnpm --dir packages/alto-sdk build
pnpm --dir packages/alto-sdk api:check
node scripts/verify-browser.mjs --suite=parity
ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --suite=parity
```

## Verification record

- `sh scripts/check.sh native` passed on Apple M5 / Metal.
- `pnpm --dir packages/alto-sdk build` and `pnpm --dir packages/alto-sdk api:check` passed after building the WASM package.
- Browser parity passed 185/185 on Metal and 185/185 on SwiftShader. The 11 Motion Blur hashes were appended to `tests/parity/v3-browser.txt` and are enforced on both backends.
- The format-8 package round-trip test passed with a Motion Blur layer and adjustment. The full native suite also covered document validation, canvas edits, exact Rust renderer reference, ROI growth, and pinned shader sources.
- Rust formatting and JavaScript syntax checks passed.
