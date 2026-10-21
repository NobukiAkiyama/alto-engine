import { installM6 } from "./m6-ui.js";
import { installM7 } from "./m7-ui.js";
import { installM8 } from "./m8-ui.js";
import { installM5 } from "./m5-ui.js";
import { AltoEngine } from "../../packages/alto-sdk/dist/index.js";
const $ = (s) => document.querySelector(s);
let engine,
  doc,
  selected,
  tool = "brush",
  gesture = null;
let target;
let m5UI,m6UI,m7UI,m8UI;
const status = (t) => ($("#status").textContent = t);
function fail(e) {
  $("#error").textContent = e.message || String(e);
  console.error(e);
}
const run = (fn) => Promise.resolve().then(fn).catch(fail);
function select(id) {
  selected = id;
  render();
}
function render() {
  if (!doc) return;
  $("#layers").replaceChildren();
  function add(n, depth) {
    if (n.id !== doc.view.root.id) {
      const b = document.createElement("button");
      b.className = "layer" + (n.id === selected ? " active" : "");
      b.style.paddingLeft = 8 + depth * 12 + "px";
      const text = document.createElement("span");
      text.textContent =
        (n.kind.kind === "group"
          ? "▣ "
          : n.kind.kind === "vector" ? "◇ "
          : n.kind.kind === "text" ? "Ｔ "
          : n.kind.kind === "adjustment"
            ? "◐ "
            : "") +
        n.name +
        (n.props.mask ? " ▧" : "") + (n.props.vector_mask ? " ◇▧" : "") + (n.props.clip_path ? " ⌁" : "") +
        (n.props.effects.length ? " ƒ" : "");
      b.append(text);
      b.addEventListener("click", () => select(n.id));
      $("#layers").append(b);
    }
    for (const child of [...n.children].reverse()) add(child, depth + 1);
  }
  add(doc.view.root, -1);
  $("#undo").disabled = !doc.view.history.undo;
  $("#redo").disabled = !doc.view.history.redo;
  $("#dirty").textContent = doc.view.dirty ? "未保存" : "保存済み";
  if (!selected || !doc.view.nodes.some((n) => n.id === selected))
    selected = doc.view.root.children.at(-1)?.id;
  const n = selected ? doc.view.node(selected) : null;
  if (n) {
    $("#name").value = n.name;
    $("#opacity").value = n.props.opacity;
    $("#blend").value = n.props.blend;
    $("#visible").checked = n.props.visible;
    $("#locked").checked = n.props.locked;
    const m = n.props.transform;
    $("#x").value = m[4];
    $("#y").value = m[5];
    $("#scale").value = Math.hypot(m[0], m[1]);
    $("#rotation").value = (Math.atan2(m[1], m[0]) * 180) / Math.PI;
  }
  m5UI?.refresh();
  m6UI?.refresh();
  m7UI?.refresh();
  m8UI?.refresh();
}
function attach(d) {
  $("#recovery").hidden = true;
  doc = d;
  doc.view.on("change", render);
  selected = doc.view.root.children.at(-1)?.id;
  render();
  fit();
  if (doc.view.warnings.length) fail(doc.view.warnings.join("\n"));
}
async function fresh() {
  const w = Number(prompt("幅 px", "1920"));
  if (!w) return;
  const h = Number(prompt("高さ px", "1080"));
  if (!h) return;
  attach(await engine.createDocument({ width: w, height: h }));
  await doc.transact("レイヤー追加", (tx) => {
    selected = tx.addRaster({ name: "レイヤー1" });
  });
  target = null;
  render();
}
function fit() {
  if (!doc) return;
  const r = engine.canvas.getBoundingClientRect();
  // A replacement canvas has no layout until the next frame; a zero size would zoom to 0.
  if (!r.width || !r.height) return void requestAnimationFrame(fit);
  engine.viewport.set({
    center: [doc.view.meta.width / 2, doc.view.meta.height / 2],
    zoom:
      Math.min(
        (r.width * devicePixelRatio) / doc.view.meta.width,
        (r.height * devicePixelRatio) / doc.view.meta.height,
      ) * 0.88,
    rotation: 0,
  });
  $("#view-rotation").value = 0;
}
function bindCanvas() {
  const c = engine.canvas;
  c.onpointerdown = (e) => {
    if (!doc) return;
    c.setPointerCapture(e.pointerId);
    if (tool === "text" && e.button === 0 && !e.altKey) {
      gesture = { text: true };
      run(() => m7UI.pointer(e, "down"));
      return;
    }
    if (tool === "pan" || e.button === 1 || e.altKey) {
      gesture = {
        x: e.clientX,
        y: e.clientY,
        center: [...engine.viewport.center],
      };
      return;
    }
    const maskEdit = $("#mask-edit").checked;
    if (
      !selected ||
      (!maskEdit && doc.view.node(selected).kind.kind !== "raster")
    )
      return;
    const hex = $("#color").value;
    try {
      const brush = doc.tools.begin(tool, {
        layer: selected,
        target: maskEdit ? "mask" : "content",
        tip: {
          size: Number($("#size").value),
          hardness: Number($("#hardness").value),
        },
        color: maskEdit
          ? $("#mask-paint").value === "white"
            ? [1, 1, 1, 1]
            : [0, 0, 0, 1]
          : [1, 3, 5]
              .map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
              .concat(1),
        flow: Number($("#flow").value),
      });
      gesture = { brush };
      brush.input(e);
    } catch (err) {
      fail(err);
    }
  };
  c.onpointermove = (e) => {
    if (!gesture) return;
    if (gesture.text) run(() => m7UI.pointer(e, "move"));
    else if (gesture.brush) gesture.brush.input(e);
    else {
      const ratio = Math.min(devicePixelRatio, 2) / engine.viewport.zoom,
        dx = (e.clientX - gesture.x) * ratio,
        dy = (e.clientY - gesture.y) * ratio;
      const a = engine.viewport.rotation;
      engine.viewport.set({
        center: [
          gesture.center[0] - Math.cos(a) * dx - Math.sin(a) * dy,
          gesture.center[1] + Math.sin(a) * dx - Math.cos(a) * dy,
        ],
      });
    }
  };
  c.onpointerup = (e) => {
    const g = gesture;
    gesture = null;
    if (g?.text) run(() => m7UI.pointer(e, "up"));
    if (g?.brush) run(() => g.brush.end());
  };
  c.onpointercancel = () => {
    if (gesture?.brush) run(() => gesture.brush.cancel());
    gesture = null;
  };
  c.onwheel = (e) => {
    e.preventDefault();
    const before = engine.viewport.screenToDocument(e.clientX, e.clientY);
    engine.viewport.set({
      zoom: Math.min(
        64,
        Math.max(0.01, engine.viewport.zoom * Math.exp(-e.deltaY * 0.001)),
      ),
    });
    const after = engine.viewport.screenToDocument(e.clientX, e.clientY);
    engine.viewport.set({
      center: [
        engine.viewport.center[0] + before[0] - after[0],
        engine.viewport.center[1] + before[1] - after[1],
      ],
    });
  };
}
function patch(p) {
  if (selected)
    return doc.transact("レイヤー設定", (tx) => tx.setProps(selected, p));
}
$("#new").onclick = () => run(fresh);
$("#add-layer").onclick = () =>
  run(async () => {
    await doc.transact("レイヤー追加", (tx) => {
      const n = selected && doc.view.node(selected);
      selected = tx.addRaster({
        parent: n?.kind.kind === "group" ? n.id : doc.view.root.id,
      });
    });
    render();
  });
$("#add-group").onclick = () =>
  run(async () => {
    await doc.transact(
      "グループ追加",
      (tx) => (selected = tx.addGroup({ isolated: true })),
    );
    render();
  });
$("#delete").onclick = () =>
  run(() => doc.transact("レイヤー削除", (tx) => tx.removeNode(selected)));
for (const [id, delta] of [
  ["move-up", 1],
  ["move-down", -1],
])
  $("#" + id).onclick = () =>
    run(() => {
      const n = doc.view.node(selected),
        p = doc.view.node(n.parent),
        i = p.children.findIndex((c) => c.id === selected);
      return doc.transact("並べ替え", (tx) =>
        tx.moveNode(
          selected,
          p.id,
          Math.max(0, Math.min(p.children.length - 1, i + delta)),
        ),
      );
    });
$("#undo").onclick = () => run(() => doc.undo());
$("#redo").onclick = () => run(() => doc.redo());
$("#fit").onclick = fit;
$("#import").onclick = () => $("#image-file").click();
$("#image-file").onchange = () =>
  run(async () => {
    const f = $("#image-file").files[0];
    if (!f) return;
    const a = await engine.assets.importImage(f);
    await doc.transact(
      "画像を配置",
      (tx) => (selected = tx.addRasterFromAsset(a, { name: f.name })),
    );
    render();
    $("#image-file").value = "";
  });
$("#open").onclick = () => $("#alto-file").click();
$("#alto-file").onchange = () =>
  run(async () => {
    const f = $("#alto-file").files[0];
    if (f) {
      attach(await engine.openDocument(f));
      target = null;
    }
    $("#alto-file").value = "";
  });
$("#save").onclick = () =>
  run(async () => {
    if (!target)
      target = await engine.storage.pickSaveFile({
        suggestedName: "work.alto",
      });
    target = await doc.save(target);
  });
for (const id of ["name", "opacity", "blend", "visible", "locked"])
  $("#" + id).onchange = () =>
    run(() =>
      patch({
        [id]: ["visible", "locked"].includes(id)
          ? $("#" + id).checked
          : id === "opacity"
            ? Number($("#" + id).value)
            : $("#" + id).value,
      }),
    );
for (const id of ["x", "y", "scale", "rotation"])
  $("#" + id).onchange = () =>
    run(() =>
      doc.transact("変形", (tx) =>
        tx.setTransform(selected, {
          x: Number($("#x").value),
          y: Number($("#y").value),
          scaleX: Number($("#scale").value),
          scaleY: Number($("#scale").value),
          rotation: (Number($("#rotation").value) * Math.PI) / 180,
        }),
      ),
    );
$("#view-rotation").onchange = () =>
  engine.viewport.set({
    rotation: (Number($("#view-rotation").value) * Math.PI) / 180,
  });
$("#size").oninput = () => ($("#size-label").textContent = $("#size").value);
for (const b of document.querySelectorAll("[data-tool]"))
  b.onclick = () => {
    tool = b.dataset.tool;
    status(
      tool === "brush"
        ? "ブラシで描画"
        : tool === "eraser"
          ? "消しゴムで描画"
          : tool === "text"
            ? "テキストレイヤー上をドラッグして文字を選択"
            : "ドラッグで移動",
    );
  };
$("#gpu-loss").onclick = () => run(() => engine.unstable.destroyGpu());
$("#restore").onclick = () =>
  run(async () => {
    attach(await engine.recover());
    $("#recovery").hidden = true;
  });
$("#discard").onclick = () =>
  run(async () => {
    $("#recovery").hidden = true;
    attach(await engine.createDocument({ width: 1920, height: 1080 }));
    await doc.transact("レイヤー追加", (tx) => (selected = tx.addRaster()));
  });
for (const [id, bench] of [
  ["test", false],
  ["benchmark", true],
])
  $("#" + id).onclick = () =>
    run(async () => {
      const { runTests } = await import("../../tests/e2e/browser-tests.js");
      $("#error").textContent = "";
      $("#test-results").textContent = "実行中…";
      $("#test").disabled = $("#benchmark").disabled = true;
      try {
        await runTests(
          engine,
          bench,
          (line) => ($("#test-results").textContent += "\n" + line),
        );
        attach(engine.document);
      } finally {
        $("#test").disabled = $("#benchmark").disabled = false;
      }
    });
window.addEventListener("keydown", (e) => {
  if (
    (e.ctrlKey || e.metaKey) &&
    e.key.toLowerCase() === "z" &&
    (!["INPUT", "TEXTAREA"].includes(document.activeElement.tagName) ||
      document.activeElement.id === "text-input")
  ) {
    e.preventDefault();
    run(() => (e.shiftKey ? doc.redo() : doc.undo()));
  }
});
try {
  engine = await AltoEngine.create({ canvas: $("#canvas") });
  engine.on("error", fail);
  engine.on("diagnostic", (d) => fail(JSON.stringify(d)));
  engine.on("frame", (f) =>
    status(
      `${f.pending ? "合成中 " + f.pending + " タイル · " : ""}${f.cpuMs.toFixed(1)} ms · GPU ${(f.gpuBytes / 1048576).toFixed(0)} MiB · WASM ${(f.wasmBytes / 1048576).toFixed(0)} MiB`,
    ),
  );
  engine.on("gpuLost", () => status("GPUを復旧しています…"));
  engine.on("gpuRestored", () => status("GPUを復旧しました"));
  engine.on("canvasReplaced", () => {
    bindCanvas();
    attach(engine.document);
  });
  bindCanvas();
  new ResizeObserver(() => {
    engine.resize();
  }).observe($("#stage"));
  if (engine.recoveryAvailable) {
    $("#recovery").hidden = false;
    status("復旧するか、新しく始めるか選んでください");
  } else {
    attach(await engine.createDocument({ width: 1920, height: 1080 }));
    await doc.transact(
      "レイヤー追加",
      (tx) => (selected = tx.addRaster({ name: "レイヤー1" })),
    );
    render();
  }
  m5UI = installM5({
    engine,
    getDocument: () => doc,
    getSelected: () => selected,
    select,
    attach,
    run,
    fail,
  });
  m5UI.refresh();
  m6UI=installM6({engine,getDocument:()=>doc,getSelected:()=>selected,select,attach,run,fail});
  m7UI=installM7({engine,getDocument:()=>doc,getSelected:()=>selected,select,attach,run,fail});
  m8UI=installM8({engine,getDocument:()=>doc,attach,run});
  window.altoEngine = engine;
} catch (e) {
  fail(e);
  status("起動できませんでした");
}

$("#report").onclick = () => {
  const report = $("#test-results").dataset.report;
  if (!report) return;
  const a = document.createElement("a");
  a.href = URL.createObjectURL(
    new Blob([JSON.stringify(JSON.parse(report), null, 2)], {
      type: "application/json",
    }),
  );
  a.download = "alto-verification.json";
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
};
