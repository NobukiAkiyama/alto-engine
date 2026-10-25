// The engine reports codes; this page words them in Japanese. A host keeps a table like
// this one in its own language.
export const errorText = {
  INVALID_ARGUMENT: "入力が正しくありません",
  CONFLICT: "ほかの操作と競合しました",
  NOT_FOUND: "見つかりません",
  CORRUPT_DATA: "データが壊れています",
  GPU_LOST: "GPUを失いました",
  GPU_UNSUPPORTED: "このブラウザではWebGPUを使えません",
  STORAGE: "保存できませんでした",
  INTERNAL: "内部エラーです",
  WORKER_CRASHED: "エンジンが停止しました",
  WORKER_UNAVAILABLE: "エンジンを読み込めません",
  WORKER_TIMEOUT: "エンジンから応答がありません",
};
const conflictText = {
  "tool-active": "ツールが使用中です",
  "folder-changed": "フォルダが他で変更されています",
  "folder-occupied": "フォルダには既に別の文書があります",
  "another-tab": "このブラウザでは別タブとの同時編集を防げません",
  "namespace-in-use": "同じ文書が別のタブで開かれています。先にそのタブを閉じてください",
  "recovery-pending": "復旧できる記録があります。先に復旧するか、記録を破棄してください",
};
export function describeError(e) {
  if (!e?.code) return String(e?.message || e);
  const reason = e.details?.reason && conflictText[e.details.reason];
  return (reason || errorText[e.code] || e.code) + `（${e.message}）`;
}
const warningText = {
  RECOVERED_OLDER_GENERATION: () => "新しい復旧記録が破損していたため、前の世代を開きました",
  RECOVERED_TRUNCATED: () => "最後の不完全な記録を除いて復旧しました",
  RECOVERY_DIVERGED: () => "復旧記録の再生が記録時の状態と一致しなかったため、その手前までを復旧しました",
  MISSING_TILE: (d) => `タイル ${d.hash?.slice(0, 10)} がありません`,
  PACKAGE_DAMAGED: (d) => `文書の一部が壊れています（${d.detail}）`,
  COMMIT_DAMAGED: (d) => `版 ${d.commit?.slice(0, 10)} が壊れています`,
  COMMIT_SNAPSHOT_MISSING: (d) => `版 ${d.commit?.slice(0, 10)} の内容がありません`,
  HISTORY_OBJECT_MISSING: (d) => `版の履歴のデータ ${d.hash?.slice(0, 10)} がありません`,
  FONT_EMBEDDING_RESTRICTED: (d) =>
    d.level === "restricted"
      ? `フォント「${d.family}」は埋め込みが禁止されています（OS/2 fsType）`
      : `フォント「${d.family}」は閲覧・印刷用の埋め込みのみ許可されています`,
  FONT_SUBSETTING_FORBIDDEN: (d) => `フォント「${d.family}」はサブセット化が許可されていません`,
  FONT_MISSING: (d) => `フォント「${d.family}」のデータがありません`,
};
export const describeWarning = (w) => (warningText[w.code] || (() => w.code))(w.details || {});
export const intentText = {
  "layer.add": "レイヤー追加",
  "layer.remove": "レイヤー削除",
  "layer.move": "並べ替え",
  "layer.props": "レイヤー設定",
  "layer.transform": "変形",
  "layer.fill": "塗り",
  "layer.gradient": "グラデーション",
  "layer.stamp": "スタンプ",
  "layer.smudge": "指先",
  "layer.rasterize": "ラスタライズ",
  "layer.merge": "結合",
  "brush.stroke": "ブラシ",
  "selection.set": "選択範囲",
  "mask.set": "マスク",
  "effects.set": "効果",
  "text.edit": "テキスト編集",
  "text.input": "文字入力",
  "text.style": "文字書式",
  "text.tate-chu-yoko": "縦中横",
  "vector.edit": "パス編集",
  "vector.mask": "ベクターマスク",
  "vector.clip": "クリッピングパス",
  "image.import": "画像を配置",
  "image.crop": "切り抜き",
  "image.rotate": "回転",
  "image.flip": "反転",
  "image.resize": "画像サイズ",
  "version.restore": "版を復元",
  "recovered.edit": "復旧した編集",
};
/** An intent from history or a commit, in Japanese. */
export function describeIntent({ intent, label, undone }) {
  const name = label || intentText[intent] || intent;
  return undone ? "取り消し: " + name : name;
}
