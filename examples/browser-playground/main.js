import { installM6 } from "./m6-ui.js";
import { installM7 } from "./m7-ui.js";
import { installM8 } from "./m8-ui.js";
import { installM5 } from "./m5-ui.js";
import { describeError, describeWarning } from "./messages.js";
import { createEngine } from "../../packages/alto-sdk/dist/index.js";
import { pickSaveTarget } from "../../packages/alto-sdk/dist/browser.js";
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
const $ = (s) => document.querySelector(s);
let engine,
  doc,
  unsubscribe = null,
  selected,
  tool = "brush",
  gesture = null;
let target;
let m5UI, m6UI, m7UI, m8UI;
const status = (t) => ($("#status").textContent = t);
function fail(e) {
  $("#error").textContent = describeError(e);
  console.error(e);
}
const run = (fn) => Promise.resolve().then(fn).catch(fail);
const snapshot = () => doc.getSnapshot();
const node = (id) => snapshot().nodes.get(id);
function select(id) {
  selected = id;
  render();
}
function render() {
  if (!doc) return;
  const s = snapshot();
  $("#layers").replaceChildren();
  function add(id, depth) {
    const n = s.nodes.get(id);
    if (!n) return;
    if (id !== s.root) {
      const b = document.createElement("button");
      b.className = "layer" + (id === selected ? " active" : "");
      b.style.paddingLeft = 8 + depth * 12 + "px";
      const text = document.createElement("span");
      text.textContent =
        ({ group: "▣ ", vector: "◇ ", text: "Ｔ ", adjustment: "◐ " }[n.type] || "") +
        n.name +
        (n.props.mask ? " ▧" : "") +
        (n.props.vector_mask ? " ◇▧" : "") +
        (n.props.clip_path ? " ⌁" : "") +
        (n.props.effects.length ? " ƒ" : "");
      b.append(text);
      b.addEventListener("click", () => select(id));
      $("#layers").append(b);
    }
    if (n.type === "group") for (const child of [...n.children].reverse()) add(child, depth + 1);
  }
  add(s.root, -1);
  $("#undo").disabled = !s.history.undo;
  $("#redo").disabled = !s.history.redo;
  $("#dirty").textContent = s.dirty ? "未保存" : "保存済み";
  if (!selected || !s.nodes.has(selected)) selected = s.nodes.get(s.root).children.at(-1);
  const n = selected ? s.nodes.get(selected) : null;
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
  if (doc === d && unsubscribe) return render();
  unsubscribe?.();
  doc = d;
  unsubscribe = doc.subscribe(render);
  selected = snapshot().nodes.get(snapshot().root).children.at(-1);
  render();
  $("#view-rotation").value = 0;
  const warnings = snapshot().warnings;
  if (warnings.length) $("#error").textContent = warnings.map(describeWarning).join("\n");
}
async function fresh() {
  const w = Number(prompt("幅 px", "1920"));
  if (!w) return;
  const h = Number(prompt("高さ px", "1080"));
  if (!h) return;
  attach(await engine.createDocument({ width: w, height: h }));
  await doc.transact("layer.add", (tx) => {
    selected = tx.addRaster({ name: "レイヤー1" });
  });
  target = null;
  render();
}
// Input goes to the container, which outlives the engine's canvas across restarts.
function bindInput(host) {
  host.onpointerdown = (e) => {
    if (!doc) return;
    if (!(tool === "pan" || e.button === 1 || e.altKey) && !engine.presentation.interactionReady) return;
    host.setPointerCapture(e.pointerId);
    if (tool === "text" && e.button === 0 && !e.altKey) {
      gesture = { text: true };
      run(() => m7UI.pointer(e, "down"));
      return;
    }
    if (tool === "pan" || e.button === 1 || e.altKey) {
      gesture = { x: e.clientX, y: e.clientY };
      return;
    }
    const maskEdit = $("#mask-edit").checked;
    if (!selected || (!maskEdit && node(selected)?.type !== "raster")) return;
    const hex = $("#color").value;
    try {
      const brush = doc.tools.begin(tool, {
        layer: selected,
        target: maskEdit ? "mask" : "content",
        tip: { size: Number($("#size").value), hardness: Number($("#hardness").value) },
        color: maskEdit
          ? $("#mask-paint").value === "white"
            ? [1, 1, 1, 1]
            : [0, 0, 0, 1]
          : [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).concat(1),
        flow: Number($("#flow").value),
      });
      gesture = { brush };
      brush.input(e);
    } catch (err) {
      fail(err);
    }
  };
  host.onpointermove = (e) => {
    if (!gesture) return;
    if (gesture.text) run(() => m7UI.pointer(e, "move"));
    else if (gesture.brush) gesture.brush.input(e);
    else {
      engine.viewport.panBy(e.clientX - gesture.x, e.clientY - gesture.y);
      gesture.x = e.clientX;
      gesture.y = e.clientY;
    }
  };
  host.onpointerup = (e) => {
    const g = gesture;
    gesture = null;
    if (g?.text) run(() => m7UI.pointer(e, "up"));
    if (g?.brush) run(() => g.brush.end());
  };
  host.onpointercancel = () => {
    if (gesture?.brush) run(() => gesture.brush.cancel());
    gesture = null;
  };
  host.onwheel = (e) => {
    e.preventDefault();
    const zoom = engine.viewport.get().zoom;
    const next = Math.min(32, Math.max(0.005, zoom * Math.exp(-e.deltaY * 0.001)));
    engine.viewport.zoomAt(e.clientX, e.clientY, next / zoom);
  };
}
function patch(p) {
  if (selected) return doc.transact("layer.props", (tx) => tx.setProps(selected, p));
}
$("#new").onclick = () => run(fresh);
$("#add-layer").onclick = () =>
  run(async () => {
    await doc.transact("layer.add", (tx) => {
      const n = selected && node(selected);
      selected = tx.addRaster({ parent: n?.type === "group" ? n.id : snapshot().root });
    });
    render();
  });
$("#add-group").onclick = () =>
  run(async () => {
    await doc.transact("layer.add", (tx) => (selected = tx.addGroup({ isolated: true })));
    render();
  });
$("#delete").onclick = () => run(() => doc.transact("layer.remove", (tx) => tx.removeNode(selected)));
for (const [id, delta] of [
  ["move-up", 1],
  ["move-down", -1],
])
  $("#" + id).onclick = () =>
    run(() => {
      const n = node(selected),
        p = node(n.parent),
        i = p.children.indexOf(selected);
      return doc.transact("layer.move", (tx) =>
        tx.moveNode(selected, p.id, Math.max(0, Math.min(p.children.length - 1, i + delta))),
      );
    });
$("#undo").onclick = () => run(() => doc.undo());
$("#redo").onclick = () => run(() => doc.redo());
$("#fit").onclick = () => {
  engine.viewport.fit();
  $("#view-rotation").value = 0;
};
$("#import").onclick = () => $("#image-file").click();
$("#image-file").onchange = () =>
  run(async () => {
    const f = $("#image-file").files[0];
    if (!f) return;
    const a = await engine.assets.importImage(f);
    await doc.transact("image.import", (tx) => (selected = tx.addRasterFromAsset(a, { name: f.name })));
    render();
    $("#image-file").value = "";
  });
$("#open").onclick = () => $("#alto-file").click();
$("#alto-file").onchange = () =>
  run(async () => {
    const f = $("#alto-file").files[0];
    if (f) {
      attach(await engine.open(f));
      target = null;
    }
    $("#alto-file").value = "";
  });
$("#save").onclick = () =>
  run(async () => {
    target ||= await pickSaveTarget({ suggestedName: "work.alto" });
    await doc.save(target);
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
      doc.transact("layer.transform", (tx) =>
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
  engine.viewport.set({ rotation: (Number($("#view-rotation").value) * Math.PI) / 180 });
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
$("#gpu-loss").onclick = () =>
  run(async () => {
    const { testing } = await import("../../packages/alto-sdk/dist/testing.js");
    await testing(engine).destroyGpu();
  });
$("#restore").onclick = () =>
  run(async () => {
    attach(await engine.recovery.restore());
    $("#recovery").hidden = true;
  });
$("#discard").onclick = () =>
  run(async () => {
    $("#recovery").hidden = true;
    await engine.recovery.discard?.();
    attach(await engine.createDocument({ width: 1920, height: 1080 }));
    await doc.transact("layer.add", (tx) => (selected = tx.addRaster()));
  });
const lock = (on) => {
  for (const selector of ["header", "nav", "main"]) $(selector).inert = on;
};
function showStorageFailure(error) {
  gesture = null;
  $("#storage-failure").hidden = false;
  const damaged = error.code === "CORRUPT_DATA";
  $("#storage-failure p").textContent = damaged
    ? "復旧記録の両世代を読み取れません。編集を停止しました。保存済みの .alto ファイルを開いて再開してください。"
    : "保存状態を確認できません。最後の編集が記録されていない可能性があります。編集を停止しました。現在の内容を退避してから復旧してください。";
  $("#emergency-export").hidden = damaged;
  $("#recover-storage").hidden = damaged;
  $("#open-recovery-file").hidden = !damaged;
  $("#recovery-result").hidden = true;
  lock(true);
  status("保存障害：編集を停止しました");
  fail(error);
}
function showEngineFailure(error) {
  gesture = null;
  $("#engine-failure").hidden = false;
  $("#engine-failure-message").textContent =
    error.code === "WORKER_TIMEOUT"
      ? "エンジンから応答がありません。接続を確認して再試行してください。"
      : error.code === "STORAGE"
        ? "保存記録を読み取れません。保存先を確認して再試行してください。"
        : error.code === "GPU_UNSUPPORTED" || error.code === "GPU_LOST"
          ? "GPUを初期化できません。ブラウザやGPUの状態を確認して再試行してください。"
          : "エンジンを読み込めません。接続を確認して再試行してください。";
  lock(true);
  status("エンジン停止：編集を停止しました");
  fail(error);
}
function onStatus({ state, error }) {
  if (state === "restarting") {
    gesture = null;
    lock(true);
    status("エンジンを復旧しています…");
  } else if (state === "storage-failed") showStorageFailure(error);
  else if (state === "failed") showEngineFailure(error);
  else if (state === "ready") {
    if (engine.document) attach(engine.document);
    $("#engine-failure").hidden = true;
    $("#storage-failure").hidden = true;
    $("#error").textContent = "";
    lock(false);
  }
}
$("#retry-worker").onclick = () =>
  run(async () => {
    if (!engine || engine.status.state !== "failed") {
      location.reload();
      return;
    }
    $("#retry-worker").disabled = true;
    try {
      await engine.retry();
    } finally {
      $("#retry-worker").disabled = false;
    }
  });
$("#open-recovery-file").onclick = () => $("#alto-file").click();
$("#emergency-export").onclick = () =>
  run(async () => {
    // The picker needs a user gesture; ask before awaiting the Worker.
    const destination = await pickSaveTarget({ suggestedName: "alto-emergency.alto" });
    const blob = await engine.recovery.exportEmergency();
    await destination.write(blob.stream());
    status("現在の内容を退避しました。記録から復旧できます");
  });
$("#recover-storage").onclick = () =>
  run(async () => {
    $("#recover-storage").disabled = true;
    try {
      const result = await engine.recovery.recoverAfterStorageFailure();
      $("#storage-failure").hidden = true;
      lock(false);
      $("#error").textContent = "";
      attach(result.document);
      const notice = $("#recovery-result");
      notice.classList.toggle("warning", result.matchesDisplay !== true);
      notice.textContent =
        result.matchesDisplay === false
          ? `障害前の表示（改訂 ${result.beforeRevision}）と復旧結果（改訂 ${result.recoveredRevision}）が異なります。退避ファイルも確認してください。`
          : result.matchesDisplay === true
            ? `記録から改訂 ${result.recoveredRevision} を復旧しました。障害前の表示と一致しています。`
            : `記録から改訂 ${result.recoveredRevision} を復旧しました。障害前の表示と比較できないため、内容を確認してください。`;
      notice.hidden = false;
    } finally {
      $("#recover-storage").disabled = false;
    }
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
        await runTests(engine, bench, (line) => ($("#test-results").textContent += "\n" + line));
        attach(engine.document);
      } finally {
        $("#test").disabled = $("#benchmark").disabled = false;
      }
    });
window.addEventListener("keydown", (e) => {
  if (engine?.status.state !== "ready") return;
  if (
    (e.ctrlKey || e.metaKey) &&
    e.key.toLowerCase() === "z" &&
    (!["INPUT", "TEXTAREA"].includes(document.activeElement.tagName) || document.activeElement.id === "text-input")
  ) {
    e.preventDefault();
    run(() => (e.shiftKey ? doc.redo() : doc.undo()));
  }
});
try {
  engine = await createEngine({ container: $("#canvas-host") });
  engine.on("error", fail);
  engine.on("status", onStatus);
  engine.on("presentation", ({phase}) => { if(phase!=="ready") status(phase==="preparing" ? "表示を準備中…" : "表示を更新中…"); });
  engine.on("document", (d) => d && attach(d));
  const diagnostics = unstable(engine);
  diagnostics.onDiagnostic((d) => (d.panic || d.message ? console.error("Worker diagnostic", d) : null));
  diagnostics.onFrame(
    (f) =>
      engine.status.state === "ready" &&
      status(
        `${f.pending ? "合成中 " + f.pending + " タイル · " : ""}${f.cpuMs.toFixed(1)} ms · GPU ${(f.gpuBytes / 1048576).toFixed(0)} MiB (固定 ${(f.gpu.fixed / 1048576).toFixed(1)} / 効果 ${(f.gpu.effects / 1048576).toFixed(1)} / 筆 ${(f.gpu.stroke / 1048576).toFixed(1)} / 図形 ${(f.gpu.vector / 1048576).toFixed(1)} / 読戻 ${(f.gpu.readback / 1048576).toFixed(1)}) · WASM ${(f.wasmBytes / 1048576).toFixed(0)} MiB`,
      ),
  );
  diagnostics.onGpu((e) => status(e === "lost" ? "GPUを復旧しています…" : "GPUを復旧しました"));
  bindInput($("#canvas-host"));
  if (engine.recovery.available) {
    $("#recovery").hidden = false;
    status("復旧するか、新しく始めるか選んでください");
  } else {
    attach(await engine.createDocument({ width: 1920, height: 1080 }));
    await doc.transact("layer.add", (tx) => (selected = tx.addRaster({ name: "レイヤー1" })));
    render();
  }
  const context = { engine, getDocument: () => doc, getSelected: () => selected, select, attach, run, fail };
  m5UI = installM5(context);
  m5UI.refresh();
  m6UI = installM6(context);
  m7UI = installM7(context);
  m8UI = installM8(context);
  window.altoEngine = engine;
} catch (e) {
  fail(e);
  status("起動できませんでした");
  if (["WORKER_UNAVAILABLE", "WORKER_TIMEOUT"].includes(e?.code)) showEngineFailure(e);
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
