const $ = (s) => document.querySelector(s);
const short = (h) => h.slice(0, 10);
const time = (ms) => new Date(ms).toLocaleString("ja-JP", { dateStyle: "short", timeStyle: "short" });
/** One readable line per change, layer by layer. */
export function describe(item, names) {
  const n = (id) => (names[id] ? `「${names[id]}」` : "ルート");
  switch (item.type) {
    case "added": return `＋ 追加 [${item.kind}] ${item.name}（${n(item.parent)}内）`;
    case "removed": return `－ 削除 [${item.kind}] ${item.name}`;
    case "moved": return `→ 移動 ${item.name}: ${n(item.from[0])}[${item.from[1]}] → ${n(item.to[0])}[${item.to[1]}]`;
    case "props_changed": return `～ 属性 ${item.name}: ${item.fields.join(", ")}`;
    case "pixels_changed": {
      const r = item.document_bounds;
      return `～ 画素 ${item.name}${item.target === "mask" ? "（マスク）" : ""}: ${item.tiles}タイル (${r.x0},${r.y0})–(${r.x1},${r.y1})`;
    }
    case "content_changed": return `～ 内容 [${item.kind}] ${item.name}: ${item.detail}`;
    case "asset_changed": return `～ アセット ${item.kind} ${{ added: "追加", removed: "削除" }[item.change] || "変更"}`;
    case "meta_changed": return `～ 文書 ${item.fields.join(", ")}`;
    case "selection_changed": return `～ 選択範囲 ${item.tiles}タイル`;
    default: return JSON.stringify(item);
  }
}
export function installM8({ engine, getDocument, attach, run }) {
  let chosen = null, log = [], folder = null, ticket = 0;
  const intents = (c) => c.intents.map((i) => `${i.intent}${i.count > 1 ? " ×" + i.count : ""}${i.target_names.length ? "（" + i.target_names.join("、") + "）" : ""}`).join(" / ");
  function showDiff(diff, empty) {
    const items = diff.items.map((i) => { const li = document.createElement("li"); li.textContent = describe(i, diff.names); return li; });
    if (!items.length) { const li = document.createElement("li"); li.textContent = empty; items.push(li); }
    $("#version-diff-list").replaceChildren(...items);
  }
  async function refresh() {
    const d = getDocument(), seq = ++ticket;
    for (const id of ["version-commit", "save-folder", "test-m8"]) $("#" + id).disabled = !d;
    if (!d) return;
    const s = d.versions.status();
    $("#version-state").textContent = s.head
      ? `${s.commits}版 · 最新 ${short(s.head)} · ${s.changed ? `作業中の変更 ${s.changes}件` : "最新の版と同じ"}`
      : "コミットはまだありません";
    $("#version-working").disabled = !s.head;
    log = await d.versions.log();
    if (seq !== ticket) return;
    if (chosen && !log.some((c) => c.hash === chosen)) chosen = null;
    $("#version-list").replaceChildren(...log.map((c) => {
      const li = document.createElement("li");
      li.className = c.hash === chosen ? "active" : "";
      li.tabIndex = 0;
      const title = document.createElement("strong"); title.textContent = c.message || "（メッセージなし）";
      const meta = document.createElement("span"); meta.className = "meta"; meta.textContent = `${short(c.hash)} · ${time(c.created_at)}${c.author ? " · " + c.author : ""}`;
      const what = document.createElement("span"); what.className = "meta"; what.textContent = intents(c);
      li.append(title, meta, what);
      li.onclick = () => { chosen = c.hash; void refresh(); };
      return li;
    }));
    $("#version-diff").disabled = $("#version-restore").disabled = !chosen;
  }
  $("#version-commit").onclick = () => run(async () => {
    const d = getDocument();
    const c = await d.versions.commit($("#commit-message").value.trim() || "無題の版");
    $("#commit-message").value = "";
    chosen = c.hash;
    await refresh();
  });
  $("#version-working").onclick = () => run(async () => {
    const d = getDocument(), head = d.versions.status().head;
    showDiff(await d.versions.diff(head, "working"), "最新の版から変更はありません");
  });
  $("#version-diff").onclick = () => run(async () => {
    const d = getDocument(), c = log.find((x) => x.hash === chosen);
    if (!c) return;
    if (c.parents[0]) showDiff(await d.versions.diff(c.parents[0], c.hash), "変更はありません");
    else showDiff({ items: [{ type: "meta_changed", fields: ["最初の版"] }], names: {} }, "");
  });
  $("#version-restore").onclick = () => run(async () => {
    const d = getDocument();
    await d.versions.restore(chosen);
    showDiff(await d.versions.diff(chosen, "working"), "選択した版の内容に戻しました（元に戻すで取り消せます）");
  });
  $("#save-folder").onclick = () => run(async () => {
    const d = getDocument();
    folder = d.folder?.provider || folder || (await engine.storage.pickFolder());
    const r = await d.saveFolder(folder, { overwrite: !d.folder });
    $("#version-state").textContent += ` · フォルダへ保存（新規 ${r.written} / 既存 ${r.skipped}）`;
  });
  $("#open-folder").onclick = () => run(async () => {
    const provider = await engine.storage.pickFolder();
    attach(await engine.openFolder(provider));
    folder = provider;
  });
  $("#test-m8").onclick = () => run(async () => {
    const b = $("#test-m8"); b.disabled = true; $("#error").textContent = ""; $("#test-results").textContent = "M8を検証中…";
    try { const { runM8Tests } = await import("../../tests/e2e/m8-tests.js"); await runM8Tests(engine, (l) => ($("#test-results").textContent += "\n" + l)); attach(engine.document); }
    finally { b.disabled = false; }
  });
  void refresh();
  return { refresh: () => void refresh().catch(() => {}) };
}
