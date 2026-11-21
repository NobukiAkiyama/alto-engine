# 対応ブラウザー

Alto Engine は、WebGPUが利用できる **Chrome、Edge、Firefox、Safari** をサポート対象とします。最新版の安定版を推奨します。ブラウザー本体に加え、OS・GPUドライバー・WebGPUの有効状態が利用可否や性能に影響します。WebGPUを利用できない環境ではPlaygroundの描画エンジンを起動できません。

## 機能とブラウザーAPI

| 機能 | Chrome / Edge | Firefox / Safari |
|---|---|---|
| WebGPUによる描画・編集 | 対応 | 対応（WebGPUが有効な環境） |
| `.alto` ファイルの保存・読み込み | 対応 | 対応。ファイルピッカー非対応時はダウンロード／ファイル選択を使用 |
| OPFSによる自動復旧・最近使ったファイル | 対応 | 対応 |
| `.altoproj` フォルダーの直接選択・増分保存 | File System Access API経由で対応 | Playgroundでは非対応。`.alto` ファイルを使用 |
| SDKのフォルダー操作 | ブラウザーAPIに応じる | `opfsFolder` など利用可能なプロバイダーを使用 |

フォルダーを直接選択する機能はブラウザー標準APIの差に依存します。SDKの `fileHandleTarget` などネイティブハンドルを使う場合は、対象ブラウザーがそのAPIを実装している必要があります。最近使ったファイルの履歴はブラウザーのオリジン内に保存されます。サイトデータを消去すると履歴や復旧用データも消去されます。

## 検証状況

2026-11-21時点でSafari 27.2、Firefox 160、Chromium 155をmacOS上で実行して確認しました。バージョン番号は検証時点の記録で、最低対応バージョンを示すものではありません。

- SafariとFirefox：基本機能スイート各56件、画像バージョンの画素検証各30件が成功。
- Safari：最近使ったファイル5件、M5 14件、M6 11件も成功。
- Chromium：基本機能57件と画像バージョン30件が成功。先行実行では全機能433件、Rust 538件、Node SDK 82件、ベクター比較10件、埋め込み例2件が成功。
- Safari/Firefox：M7/M8と191ケースの描画パリティは先行検証で成功。

この検証は特定のMac・OS・GPU環境での実行結果です。Firefox/Safariを含むブラウザー行列は現在のCIでは自動実行していません。CIのブラウザー検証はChrome for TestingとSwiftShaderを使います。上記の結果は、すべてのGPU・OS・ブラウザー版での一致や、Chromeと同一の全スイート完走を保証するものではありません。未完了の項目や新しい失敗は[Issue #168](https://github.com/NobukiAkiyama/alto-engine/issues/168)で追跡します。

## 手元で確認する

リポジトリルートでサーバーを起動し、対象ブラウザーでPlaygroundを開きます。

```sh
sh scripts/build.sh
sh scripts/run.sh
```

`http://localhost:8765/examples/browser-playground/` にアクセスし、「検証と計測」から機能テストとM5〜M8のテストを実行します。ブラウザーの開発者コンソールにWebGPUが利用できない旨が表示される場合は、ブラウザーのWebGPU設定とGPUドライバーを確認してください。

自動ブラウザー検証スクリプト `node scripts/verify-browser.mjs --functional` は、現状Chromeを起動します。Safari/Firefoxの確認は各ブラウザーでPlaygroundを開いて行ってください。
