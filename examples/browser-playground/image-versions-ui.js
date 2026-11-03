/** Thin controls for the public image/history APIs. */
export function installImageVersions({ engine, getDocument, getSelected, select, attach, run }) {
  document.querySelector("#test-image-versions").onclick = () => run(async () => {
    const button = document.querySelector("#test-image-versions"), result = document.querySelector("#test-results");
    button.disabled = true; result.textContent = "画像編集・版管理を検証中…";
    try { const { runImageVersionTests } = await import("../../tests/e2e/image-version-tests.js"); await runImageVersionTests(engine, (line) => { result.textContent += "\n" + line; }); }
    finally { button.disabled = false; attach(engine.document); }
  });
  const panel = document.createElement("section");
  panel.innerHTML = `
    <fieldset><legend>レイヤーの画像編集</legend>
      <p class="meta">座標はレイヤー内の画素です。切り抜き・拡縮・回転は画像全体に適用します。</p>
      <label>左端<input id="image-x" type="number" step="1" value="0"></label>
      <label>上端<input id="image-y" type="number" step="1" value="0"></label>
      <label>幅<input id="image-width" type="number" min="1" max="65536" step="1" value="256"></label>
      <label>高さ<input id="image-height" type="number" min="1" max="65536" step="1" value="256"></label>
      <button id="image-crop">指定範囲で切り抜く</button>
      <label>拡縮の補間<select id="image-filter"><option value="bilinear">なめらか</option><option value="nearest">最近傍（ドットを維持）</option></select></label>
      <button id="image-resize">指定サイズへ拡縮</button>
      <div class="tools"><button id="image-rotate">右へ90度回転</button><button id="image-flip-h">左右反転</button><button id="image-flip-v">上下反転</button></div>
      <label>始点・塗り始め X<input id="image-start-x" type="number" value="0"></label>
      <label>始点・塗り始め Y<input id="image-start-y" type="number" value="0"></label>
      <label>終点 X<input id="image-end-x" type="number" value="256"></label>
      <label>終点 Y<input id="image-end-y" type="number" value="0"></label>
      <label>終点の色<input id="image-end-color" type="color" value="#ffffff"></label>
      <label>グラデーション<select id="image-gradient-kind"><option value="linear">線形</option><option value="radial">円形</option></select></label>
      <label>選択範囲を使う<input id="image-selection" type="checkbox" checked></label>
      <button id="image-gradient">現在の色からグラデーション</button>
      <label>塗りつぶしの許容差<input id="image-tolerance" type="number" min="0" max="255" step="1" value="0"></label>
      <button id="image-fill">始点から塗りつぶす</button><button id="image-pick">始点のレイヤー色を取得</button>
      <p id="image-color-result" class="meta" aria-live="polite"></p>
      <label>SVGファイル<input id="image-svg" type="file" accept=".svg,image/svg+xml"></label>
    </fieldset>
    <fieldset><legend>レイヤーの筆と結合</legend>
      <p class="meta">座標はレイヤー内の画素です。筆は「始点」から「終点」へ動かします。</p>
      <label>筆の半径<input id="image-radius" type="number" min="1" max="1024" step="1" value="16"></label>
      <label>スタンプの参照元 X<input id="image-source-x" type="number" step="1" value="0"></label>
      <label>スタンプの参照元 Y<input id="image-source-y" type="number" step="1" value="0"></label>
      <button id="image-stamp">コピースタンプ</button><button id="image-smudge">指先</button>
      <button id="image-histogram">ヒストグラムを表示</button>
      <p id="image-histogram-result" class="meta" aria-live="polite"></p>
      <p class="meta">結合とラスタライズは、見た目を保てない構造（マスク・効果・合成モードなど）では何も変更しません。</p>
      <button id="image-merge-down">下のレイヤーと結合</button><button id="layer-rasterize">位置を画素へ確定 / グループを1枚に</button>
    </fieldset>
    <fieldset><legend>キャンバス全体</legend>
      <p class="meta">全レイヤーと選択範囲が一緒に動きます。レイヤーの画素は書き換えません。</p>
      <label>左端<input id="canvas-x" type="number" step="1" value="0"></label>
      <label>上端<input id="canvas-y" type="number" step="1" value="0"></label>
      <label>幅<input id="canvas-width" type="number" min="1" max="65536" step="1" value="512"></label>
      <label>高さ<input id="canvas-height" type="number" min="1" max="65536" step="1" value="512"></label>
      <button id="canvas-crop">この範囲をキャンバスにする</button><button id="canvas-resize">この幅・高さへ拡縮</button>
      <div class="tools"><button id="canvas-rotate">右へ90度回転</button><button id="canvas-flip-h">左右反転</button><button id="canvas-flip-v">上下反転</button></div>
    </fieldset>
    <fieldset><legend>ブランチと統合</legend>
      <p class="meta">切替には未コミットの変更がない状態が必要です。切替・内容が変わる統合を行うと、元に戻す履歴はクリアされます。</p>
      <p id="branch-current" class="meta" aria-live="polite"></p>
      <label>ブランチ<select id="branch-target"></select></label>
      <p class="meta">切替・統合の前に変更を版へ記録してください。切替・内容を変える統合は「元に戻す」の履歴をクリアします。</p>
      <button id="branch-checkout">切り替える</button>
      <label>新しい名前<input id="branch-name" maxlength="128" placeholder="別案"></label>
      <label>分岐元の版（空欄は現在の版）<input id="branch-from" placeholder="版のID"></label>
      <button id="branch-create">分岐を作って切り替える</button>
      <label>削除を保護<input id="branch-protected" type="checkbox"></label><button id="branch-protect">保護設定を適用</button>
      <label>未統合でも削除する<input id="branch-force" type="checkbox"></label><button id="branch-delete">選んだブランチを削除</button>
      <button id="branch-merge">選んだブランチとの統合を準備</button>
      <div id="merge-conflicts"></div><button id="merge-finish" disabled>解決内容で統合を確定</button><button id="merge-cancel" disabled>統合をキャンセル</button>
      <p id="merge-result" class="meta" aria-live="polite"></p>
    </fieldset>
    <fieldset><legend>未使用データの整理</legend>
      <p class="meta">先にフォルダへ保存してください。参照される過去の版は残します。</p>
      <button id="gc-preview">削除候補を確認</button><button id="gc-execute" disabled>確認した候補を削除</button>
      <p id="gc-report" class="meta" aria-live="polite"></p>
    </fieldset>`;
  document.querySelector("#effects-panel").before(panel);
  const $ = (id) => panel.querySelector("#" + id), value = (id) => Number($(id).value);
  const target = () => {
    const d = getDocument(), id = getSelected(), n = d?.getSnapshot().nodes.get(id);
    if (!d || n?.type !== "raster") throw Error("画像レイヤーを選んでください");
    return { d, id, n };
  };
  const color = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).concat(1);
  const start = () => [value("image-start-x"), value("image-start-y")];
  const selection = () => $("image-selection").checked ? "current" : "ignore";
  const bind = (id, fn) => $(id).onclick = () => run(fn);
  bind("image-crop", () => { const { d, id } = target(); return d.image.crop(id, { x: value("image-x"), y: value("image-y"), width: value("image-width"), height: value("image-height") }); });
  bind("image-resize", () => { const { d, id } = target(); return d.image.resize(id, value("image-width"), value("image-height"), { filter: $("image-filter").value }); });
  bind("image-rotate", () => { const { d, id } = target(); return d.image.rotate(id, 1); });
  for (const [button, axis] of [["image-flip-h", "horizontal"], ["image-flip-v", "vertical"]]) bind(button, () => { const { d, id } = target(); return d.image.flip(id, axis); });
  bind("image-gradient", () => { const { d, id } = target(); return d.image.gradient(id, { kind: $("image-gradient-kind").value, start: start(), end: [value("image-end-x"), value("image-end-y")], stops: [{ offset: 0, color: color(document.querySelector("#color").value) }, { offset: 1, color: color($("image-end-color").value) }], selection: selection() }); });
  bind("image-fill", () => { const { d, id } = target(); return d.image.fill(id, start(), color(document.querySelector("#color").value), { tolerance: value("image-tolerance"), selection: selection() }); });
  bind("image-pick", async () => { const { d, id } = target(); const p = await d.image.pickColor(id, start()); document.querySelector("#color").value = "#" + p.bytes.slice(0, 3).map((v) => v.toString(16).padStart(2, "0")).join(""); $("image-color-result").textContent = `取得した色：RGBA ${p.bytes.join(", ")}`; });
  const strokeOf = () => ({ points: [start(), [value("image-end-x"), value("image-end-y")]], radius: value("image-radius") });
  bind("image-stamp", () => { const { d, id } = target(); return d.image.cloneStamp(id, [value("image-source-x"), value("image-source-y")], strokeOf(), { selection: selection() }); });
  bind("image-smudge", () => { const { d, id } = target(); return d.image.smudge(id, strokeOf(), { selection: selection() }); });
  bind("image-histogram", async () => { const { d, id } = target(); const h = await d.image.histogram(id); const peak = (bins) => bins.indexOf(Math.max(...bins)); $("image-histogram-result").textContent = `${h.pixels}画素。最多の値 R ${peak(h.r)} / G ${peak(h.g)} / B ${peak(h.b)} / A ${peak(h.a)}。透明 ${h.a[0]}画素`; });
  bind("image-merge-down", () => { const { d, id } = target(); return d.image.mergeDown(id); });
  bind("layer-rasterize", async () => { const d = getDocument(), id = getSelected(); if (!d || !id) throw Error("レイヤーを選んでください"); const r = await d.image.rasterize(id); select(r.node); });
  bind("canvas-crop", () => getDocument().canvas.crop({ x: value("canvas-x"), y: value("canvas-y"), width: value("canvas-width"), height: value("canvas-height") }));
  bind("canvas-resize", () => getDocument().canvas.resize(value("canvas-width"), value("canvas-height")));
  bind("canvas-rotate", () => getDocument().canvas.rotate(1));
  bind("canvas-flip-h", () => getDocument().canvas.flip("horizontal"));
  bind("canvas-flip-v", () => getDocument().canvas.flip("vertical"));
  $("image-svg").onchange = () => run(async () => {
    const file = $("image-svg").files[0]; if (!file) return;
    try { if (file.size > 4 * 1024 * 1024) throw Error("SVGは4 MiB以下にしてください"); const d = getDocument(); if (!d) return; const r = await d.image.importSvg(await file.text()); select(r.node); }
    finally { $("image-svg").value = ""; }
  });
  let plan = null, planDocument = null, planState = "", preview = null, boundsKey = "", canvasKey = "", ticket = 0;
  bind("branch-create", () => getDocument().versions.createBranch($("branch-name").value, { from: $("branch-from").value.trim() || undefined, checkout: true }));
  bind("branch-checkout", () => getDocument().versions.checkout($("branch-target").value));
  bind("branch-protect", () => getDocument().versions.setBranchProtection($("branch-target").value, $("branch-protected").checked));
  bind("branch-delete", () => getDocument().versions.deleteBranch($("branch-target").value, { force: $("branch-force").checked }));
  const endMerge = () => { plan = null; planDocument = null; $("branch-target").disabled = false; $("merge-conflicts").replaceChildren(); $("merge-finish").disabled = $("merge-cancel").disabled = true; };
  bind("branch-merge", async () => {
    const d = getDocument(), sourceName = $("branch-target").value; plan = await d.versions.prepareMerge(sourceName); planDocument = d; planState = d.getSnapshot().stateHash; $("branch-target").disabled = true;
    $("merge-result").textContent = plan.kind === "no_op" ? "既に統合済みです。確定すると変更せず終了します。" : plan.kind === "fast_forward" ? "競合はありません。選んだ版まで履歴を進められます。" : `${sourceName}を現在のブランチへ統合します。競合 ${plan.conflicts.length}件を解決してから確定してください。`;
    const names = { property: "属性", pixels: "画素", geometry: "画像範囲", existence: "追加・削除", parent: "所属", order: "並び順", structure: "レイヤー構造", content: "内容", kind: "種類", asset: "素材" };
    $("merge-conflicts").replaceChildren(...plan.conflicts.map((c) => { const label = document.createElement("label"); const nodeName = d.getSnapshot().nodes.get(c.path[1])?.name;
      const fields = { name: "レイヤー名", opacity: "不透明度", visible: "表示", locked: "ロック", transform: "変形", children: "並び順", parent: "所属", bounds: "画像範囲", content: "内容", paths: "図形", effects: "効果" };
      label.textContent = `${nodeName ? nodeName + "：" : ""}${names[c.kind] || "変更"}${c.kind === "pixels" ? "（同じ領域）" : fields[c.path.at(-1)] ? "・" + fields[c.path.at(-1)] : ""}`; const select = document.createElement("select"); select.dataset.conflict = c.id; for (const [v, text] of [["", "解決方法を選ぶ"], ["ours", "現在のブランチを採用"], ["theirs", "統合するブランチを採用"]]) { const o = document.createElement("option"); o.value = v; o.textContent = text; select.append(o); } label.append(select); return label; }));
    $("merge-finish").disabled = $("merge-cancel").disabled = false;
  });
  bind("merge-finish", async () => { if (!plan) return; const choices = [...$("merge-conflicts").querySelectorAll("select")]; if (choices.some((s) => !s.value)) throw Error("すべての競合の解決方法を選んでください"); const r = await getDocument().versions.finishMerge(plan.token, choices.map((s) => ({ conflict_id: s.dataset.conflict, choice: s.value }))); endMerge(); $("merge-result").textContent = r.kind === "no_op" ? "変更はありません" : "統合を確定しました"; });
  bind("merge-cancel", async () => { await getDocument().versions.cancelMerge(); endMerge(); $("merge-result").textContent = "統合をキャンセルしました"; });
  bind("gc-preview", async () => { const d = getDocument(); preview = await d.versions.gc(); const counts = Object.values(preview.by_kind); const removed = counts.reduce((n, c) => n + c.removed, 0), bytes = counts.reduce((n, c) => n + c.removed_bytes, 0); const caps = d.folder?.gcCapabilities; const capable = typeof d.folder?.exclusive === "function" && caps?.exclusive === true && caps.atomicManifest === true && caps.deleteObjects === true; $("gc-report").textContent = `削除候補 ${removed}件、${(bytes / 1048576).toFixed(2)} MiB。${capable ? "内容を確認して実行してください。" : "この保存先は確認のみ対応しています。"}`; $("gc-execute").disabled = !capable || !removed; });
  bind("gc-execute", async () => { if (!preview) return; const r = await getDocument().versions.gc({ dryRun: false, expectedBaseline: preview.baseline }); preview = null; $("gc-execute").disabled = true; $("gc-report").textContent = `未使用データを整理しました（保護対象 ${r.roots}件）${r.warnings.length ? " · " + r.warnings.join("、") : ""}`; });
  async function refresh() {
    const seq = ++ticket, d = getDocument(); if (!d) return;
    const s = d.getSnapshot();
    if (plan && (planDocument !== d || planState !== s.stateHash)) { endMerge(); $("merge-result").textContent = "文書が変わりました。統合を準備し直してください。"; }
    const n = s.nodes.get(getSelected()), b = n?.type === "raster" ? n.bounds : null;
    for (const button of panel.querySelectorAll("button[id^='image-']")) button.disabled = !b;
    $("layer-rasterize").disabled = !n || n.type === "root";
    if (document.activeElement !== $("canvas-width") && document.activeElement !== $("canvas-height") && (canvasKey !== `${s.meta.width}x${s.meta.height}`)) { canvasKey = `${s.meta.width}x${s.meta.height}`; $("canvas-width").value = s.meta.width; $("canvas-height").value = s.meta.height; }
    const key = b ? `${n.id}:${JSON.stringify(b)}` : "";
    if (b && key !== boundsKey) { boundsKey = key; for (const [field, v] of Object.entries(b)) $("image-" + field).value = v; $("image-start-x").value = b.x; $("image-start-y").value = b.y; $("image-end-x").value = b.x + b.width; $("image-end-y").value = b.y; }
    const refs = s.versions.refs; $("branch-current").textContent = refs.current_branch ? `現在：${refs.current_branch}` : s.versions.head ? "過去の版を閲覧中" : "最初の版を記録するとブランチを作れます";
    const branches = await d.versions.listBranches(); if (seq !== ticket) return;
    const selected = $("branch-target").value || refs.current_branch;
    $("branch-target").replaceChildren(...branches.map((b) => { const o = document.createElement("option"); o.value = b.name; o.textContent = `${b.name}${b.current ? "（現在）" : ""}${b.protected ? " 🔒" : ""} +${b.ahead}/−${b.behind}`; return o; }));
    if (branches.some((b) => b.name === selected)) $("branch-target").value = selected;
    $("branch-protected").checked = branches.find((b) => b.name === $("branch-target").value)?.protected ?? false;
    $("gc-preview").disabled = !d.folder;
  }
  $("branch-target").onchange = () => { const d = getDocument(); $("branch-protected").checked = d?.getSnapshot().versions.refs.branches[$("branch-target").value]?.protected ?? false; };
  return { refresh: () => void refresh().catch(() => {}) };
}
