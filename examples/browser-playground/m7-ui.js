import { replaceText } from "../../packages/alto-sdk/dist/index.js";
const $ = (s) => document.querySelector(s);
const color = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).concat(1);
const hex = (c) => "#" + c.slice(0, 3).map((v) => Math.round(v * 255).toString(16).padStart(2, "0")).join("");
const SVG = "http://www.w3.org/2000/svg";
export function parseFeatures(s) {
  return s.split(/[\s,]+/).filter(Boolean).map((f) => {
    const [tag, v] = f.split("=");
    return { tag: tag.padEnd(4, " ").slice(0, 4), value: v === undefined ? 1 : Number(v) };
  });
}
export const TEST_FONTS = ["../../tests/fonts/AltoTestJP.otf", "../../tests/fonts/AltoTestLatin.ttf"];

export function installM7({ engine, getDocument, getSelected, select, attach, run, fail }) {
  // Text tool state: anchor/focus are UTF-16 offsets in the selected text layer.
  let sel = null, dragging = false, composing = false, lastKey = "";
  const current = () => {
    const d = getDocument(), id = getSelected();
    const n = d && id ? d.view.node(id) : null;
    return { d, id, n, text: n?.kind.kind === "text" ? n.kind.content : null };
  };
  function fontOrder() {
    const d = getDocument();
    const all = (d?.view.assets || []).filter((a) => a.kind.kind === "font").map((a) => a.id);
    const first = $("#text-font").value;
    return first ? [first, ...all.filter((f) => f !== first)] : all;
  }
  function content(base) {
    const frame = $("#text-frame").value === "area"
      ? { kind: "area", width: Number($("#text-width").value), height: Number($("#text-height").value) }
      : { kind: "point" };
    return {
      ...(base || {}),
      text: base ? base.text : $("#text-content").value,
      style: {
        ...(base?.style || {}),
        fonts: fontOrder(),
        size: Number($("#text-size").value),
        color: color($("#text-color").value),
        line_height: Number($("#text-line-height").value),
        letter_spacing: Number($("#text-spacing").value),
        features: parseFeatures($("#text-features").value),
        language: "ja",
      },
      frame,
      writing_mode: $("#text-writing").value,
      align: $("#text-align").value,
      auto_tcy: Number($("#text-tcy").value),
      spans: base?.spans || [],
      tcy: base?.tcy || [],
    };
  }
  function load(n) {
    const c = n.kind.content;
    $("#text-content").value = c.text;
    $("#text-writing").value = c.writing_mode || "horizontal_tb";
    $("#text-size").value = c.style.size;
    $("#text-line-height").value = c.style.line_height ?? 1.5;
    $("#text-spacing").value = c.style.letter_spacing ?? 0;
    $("#text-color").value = hex(c.style.color || [0, 0, 0, 1]);
    $("#text-align").value = c.align || "start";
    $("#text-frame").value = c.frame?.kind || "point";
    if (c.frame?.kind === "area") {
      $("#text-width").value = c.frame.width;
      $("#text-height").value = c.frame.height;
    }
    $("#text-tcy").value = c.auto_tcy ?? 2;
    $("#text-features").value = (c.style.features || []).map((f) => (f.value === 1 ? f.tag : `${f.tag}=${f.value}`)).join(", ");
    if (c.style.fonts[0]) $("#text-font").value = c.style.fonts[0];
  }
  async function registerFonts(files) {
    const d = getDocument();
    const imported = [];
    for (const f of files) imported.push(await engine.assets.importFont(f));
    await d.transact("フォント追加", (tx) => imported.forEach((f) => tx.addFont(f)));
    const warnings = imported.flatMap((f) => f.warnings);
    if (warnings.length) fail(new Error(warnings.join("\n")));
    refresh();
    return imported;
  }
  $("#font-load").onclick = () => $("#font-file").click();
  $("#font-file").onchange = () => run(async () => {
    const f = $("#font-file").files[0];
    if (f) {
      const [font] = await registerFonts([f]);
      $("#text-font").value = font.record.id;
    }
    $("#font-file").value = "";
  });
  $("#font-demo").onclick = () => run(async () => {
    const files = await Promise.all(TEST_FONTS.map(async (u) => new File([await (await fetch(u)).blob()], u.split("/").pop())));
    const [jp] = await registerFonts(files);
    $("#text-font").value = jp.record.id;
  });
  $("#text-add").onclick = () => run(async () => {
    const { d, n } = current();
    let id;
    await d.transact("テキスト追加", (tx) => {
      id = tx.addText(content(), { parent: n?.kind.kind === "group" ? n.id : n?.parent || d.view.root.id });
      tx.setTransform(id, [1, 0, 0, 1, 60, 60]);
    });
    select(id);
  });
  $("#text-update").onclick = () => run(async () => {
    const { d, id, text } = current();
    if (!text) throw new Error("テキストレイヤーを選択してください");
    // Map the textarea edit to one replacement so styled spans keep their place.
    const next = $("#text-content").value, prev = text.text;
    let a = 0;
    while (a < prev.length && a < next.length && prev[a] === next[a]) a++;
    let b = 0;
    while (b < prev.length - a && b < next.length - a && prev[prev.length - 1 - b] === next[next.length - 1 - b]) b++;
    const edited = replaceText(text, a, prev.length - b, next.slice(a, next.length - b));
    await d.text.set(id, content(edited));
  });

  // ---- Canvas selection, caret and keyboard / IME editing ----
  function screenMap() {
    const ratio = Math.min(devicePixelRatio || 1, 2), z = engine.viewport.zoom / ratio, a = engine.viewport.rotation;
    const c = Math.cos(a), s = Math.sin(a), r = engine.canvas.getBoundingClientRect(), [cx, cy] = engine.viewport.center;
    return { r, f: ([x, y]) => { const dx = x - cx, dy = y - cy; return [r.width / 2 + z * (c * dx - s * dy), r.height / 2 + z * (s * dx + c * dy)]; } };
  }
  let drawTicket = 0;
  async function draw() {
    const overlay = $("#text-overlay"), ticket = ++drawTicket;
    const { d, id, text } = current();
    if (!text || !d) { overlay.replaceChildren(); return; }
    const info = await d.text.info(id);
    const rects = sel && sel.id === id ? await d.text.selectionRects(id, Math.min(sel.anchor, sel.focus), Math.max(sel.anchor, sel.focus)) : [];
    const caret = sel && sel.id === id ? await d.text.caret(id, sel.focus) : null;
    if (ticket !== drawTicket) return;
    const { r, f } = screenMap();
    overlay.setAttribute("viewBox", `0 0 ${r.width} ${r.height}`);
    const poly = (q, cls) => { const e = document.createElementNS(SVG, "polygon"); e.setAttribute("points", q.map(f).map((p) => p.join(",")).join(" ")); e.setAttribute("class", cls); return e; };
    const items = [poly(info.quad, "text-frame"), ...rects.map((q) => poly(q, "text-selection"))];
    if (caret && sel.anchor === sel.focus) {
      const [a, b] = [f(caret.a), f(caret.b)], line = document.createElementNS(SVG, "line");
      line.setAttribute("x1", a[0]); line.setAttribute("y1", a[1]); line.setAttribute("x2", b[0]); line.setAttribute("y2", b[1]); line.setAttribute("class", "text-caret");
      items.push(line);
    }
    overlay.replaceChildren(...items);
    const missing = info.missing.length ? ` · 表示できない文字 ${info.missing.length}` : "";
    const chosen = sel && sel.id === id ? text.text.slice(Math.min(sel.anchor, sel.focus), Math.max(sel.anchor, sel.focus)) : "";
    $("#text-selection-state").textContent = sel && sel.id === id
      ? `選択 ${Math.min(sel.anchor, sel.focus)}–${Math.max(sel.anchor, sel.focus)}${chosen ? "「" + chosen.slice(0, 20) + "」" : "（キャレット）"} · ${info.lines.length}行${missing}`
      : `${info.lines.length}行 · ${info.glyphs}グリフ${missing}`;
  }
  async function pointer(e, phase) {
    const { d, id, text } = current();
    if (!text) { if (phase === "down") fail(new Error("テキストレイヤーを選択してから「テキスト」ツールを使ってください")); return; }
    const p = engine.viewport.screenToDocument(e.clientX, e.clientY);
    const offset = await d.text.hitTest(id, p[0], p[1]);
    if (phase === "down") { sel = { id, anchor: e.shiftKey && sel?.id === id ? sel.anchor : offset, focus: offset }; dragging = true; }
    else if (dragging) sel = { ...sel, focus: offset };
    if (phase === "up") { dragging = false; const input = $("#text-input"); input.value = ""; input.focus({ preventScroll: true }); }
    await draw();
  }
  async function edit(insert, deleteDir = 0) {
    const { d, id, text } = current();
    if (!sel || sel.id !== id || !text) return;
    let a = Math.min(sel.anchor, sel.focus), b = Math.max(sel.anchor, sel.focus);
    if (a === b && deleteDir) {
      const to = await d.text.navigate(id, a, deleteDir < 0 ? "backward" : "forward");
      [a, b] = [Math.min(a, to), Math.max(a, to)];
      if (a === b) return;
    }
    await d.text.replace(id, a, b, insert, insert ? "文字入力" : "文字削除");
    const caret = a + insert.length;
    sel = { id, anchor: caret, focus: caret };
    lastKey = "";
    $("#text-content").value = d.view.node(id).kind.content.text;
    await draw();
  }
  async function move(motion, extend) {
    const { d, id } = current();
    if (!sel || sel.id !== id) return;
    const focus = await d.text.navigate(id, sel.focus, motion);
    sel = { id, anchor: extend ? sel.anchor : focus, focus };
    await draw();
  }
  const input = $("#text-input");
  input.addEventListener("compositionstart", () => (composing = true));
  input.addEventListener("compositionend", (e) => { composing = false; const t = e.data || input.value; input.value = ""; if (t) run(() => edit(t)); });
  input.addEventListener("input", (e) => {
    if (composing || e.isComposing) return;
    const t = input.value; input.value = "";
    if (t) run(() => edit(t));
  });
  input.addEventListener("keydown", (e) => {
    if (composing || e.isComposing) return;
    const vertical = current().text?.writing_mode === "vertical_rl";
    const keys = vertical
      ? { ArrowDown: "forward", ArrowUp: "backward", ArrowLeft: "next_line", ArrowRight: "previous_line" }
      : { ArrowRight: "forward", ArrowLeft: "backward", ArrowDown: "next_line", ArrowUp: "previous_line" };
    if (keys[e.key]) { e.preventDefault(); run(() => move(keys[e.key], e.shiftKey)); }
    else if (e.key === "Home" || e.key === "End") { e.preventDefault(); run(() => move(e.key === "Home" ? "line_start" : "line_end", e.shiftKey)); }
    else if (e.key === "Backspace" || e.key === "Delete") { e.preventDefault(); run(() => edit("", e.key === "Backspace" ? -1 : 1)); }
    else if (e.key === "Enter") { e.preventDefault(); run(() => edit("\n")); }
    else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "a") { e.preventDefault(); const { text, id } = current(); if (text) { sel = { id, anchor: 0, focus: text.text.length }; run(draw); } }
    else if (e.key === "Escape") { sel = null; input.blur(); run(draw); }
  });
  $("#text-style-selection").onclick = () => run(async () => {
    const { d, id } = current();
    if (!sel || sel.id !== id || sel.anchor === sel.focus) throw new Error("文字を選択してください");
    await d.text.style(id, sel.anchor, sel.focus, { color: color($("#text-color").value), size: Number($("#text-size").value), features: parseFeatures($("#text-features").value) });
    await draw();
  });
  $("#text-tcy-selection").onclick = () => run(async () => {
    const { d, id } = current();
    if (!sel || sel.id !== id || sel.anchor === sel.focus) throw new Error("縦中横にする文字を選択してください");
    await d.text.setTateChuYoko(id, Math.min(sel.anchor, sel.focus), Math.max(sel.anchor, sel.focus));
    await draw();
  });
  function refresh() {
    const { d, n, text } = current();
    const list = (d?.view.assets || []).filter((a) => a.kind.kind === "font");
    const keep = $("#text-font").value;
    $("#text-font").replaceChildren(...list.map((a) => { const o = document.createElement("option"); o.value = a.id; o.textContent = a.kind.family || a.id.slice(0, 8); return o; }));
    if (list.some((a) => a.id === keep)) $("#text-font").value = keep;
    $("#font-state").textContent = list.length
      ? `${list.length}書体を登録済み。先頭のフォントにない文字は、登録順の次のフォントで表示します。`
      : "フォントが未登録です。文字は仮の枠で表示されます。";
    for (const b of ["text-add", "font-load", "font-demo", "test-m7", "sample-m78"]) $("#" + b).disabled = !d;
    for (const b of ["text-update", "text-style-selection", "text-tcy-selection"]) $("#" + b).disabled = !text;
    const key = (n?.id || "") + JSON.stringify(text);
    if (text && key !== lastKey) { lastKey = key; load(n); }
    if (sel && (!text || sel.id !== n.id)) sel = null;
    void draw().catch(fail);
  }
  engine.on("viewport", () => void draw().catch(() => {}));
  $("#test-m7").onclick = () => run(async () => {
    const b = $("#test-m7"); b.disabled = true; $("#error").textContent = ""; $("#test-results").textContent = "M7を検証中…";
    try { const { runM7Tests } = await import("../../tests/e2e/m7-tests.js"); await runM7Tests(engine, (l) => ($("#test-results").textContent += "\n" + l)); attach(engine.document); }
    finally { b.disabled = false; }
  });
  $("#sample-m78").onclick = () => run(async () => attach(await engine.openDocument(await (await fetch("../../verification/sample-m78.alto")).blob())));
  refresh();
  return { refresh, pointer, isActive: () => !!sel };
}
