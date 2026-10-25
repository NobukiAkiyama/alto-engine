const host = document.querySelector("#host");
const frame = () => new Promise(requestAnimationFrame);
const trace = [];
function record(event, detail = {}) {
  const entry = { at: Date.now(), event, ...detail };
  trace.push(entry);
  if (trace.length > 2000) trace.shift();
  console.log("LIFECYCLE", JSON.stringify(entry));
}
window.runGpuLifecycle = async ({ mode = "stable", cycles = 120, memoryMiB = 0, implementation = "webgpu" } = {}) => {
  if (!["stable", "same-size", "resize", "same-size-burst", "resize-burst", "replace-abrupt", "replace-graceful", "recover", "recover-unconfigure", "backlog-resize", "backlog-replace", "unused-present", "unused-replace", "recorded-startup"].includes(mode)) throw Error("unknown mode");
  if ((mode.startsWith("unused-") || mode === "recorded-startup") && implementation !== "webgpu") throw Error("surface controls require raw WebGPU");
  let worker, canvas, nextId = 0, cycle = 0, generation = 0;
  const pending = new Map();
  const faults = [];
  const request = (op, data = {}, transfer = []) => new Promise((resolve, reject) => {
    const id = ++nextId;
    const timer = setTimeout(() => { pending.delete(id); reject(Error(`timeout ${op} at cycle ${cycle}`)); }, op === "init" ? 30000 : 15000);
    pending.set(id, { resolve, reject, timer });
    worker.postMessage({ id, op, ...data }, transfer);
  });
  const start = async () => {
    ++generation;
    const gen = generation;
    canvas = document.createElement("canvas");
    canvas.width = mode === "recorded-startup" ? 300 : 800; canvas.height = mode === "recorded-startup" ? 150 : 600;
    host.replaceChildren(canvas);
    worker = new Worker(new URL("./gpu-lifecycle-worker.js", import.meta.url), { type: "module" });
    worker.onmessage = ({ data }) => {
      if (data.event) {
        record(data.event, { ...data, generation: gen, cycle });
        if (data.event === "validation" || data.event === "failure" || (data.event === "lost" && !data.expected)) faults.push(data);
        if (data.event === "adapter" && !data.architecture.includes("swiftshader")) faults.push({ message: "SwiftShader was not selected" });
        return;
      }
      const p = pending.get(data.id);
      if (!p) return;
      clearTimeout(p.timer); pending.delete(data.id);
      data.ok ? p.resolve(data.value) : p.reject(Error(data.error));
    };
    worker.onerror = (e) => faults.push({ message: e.message });
    const offscreen = canvas.transferControlToOffscreen();
    await request("init", { canvas: offscreen, memoryMiB, implementation, backlog: mode.startsWith("backlog-"), replay: mode === "recorded-startup" }, [offscreen]);
    record("start", { generation, cycle });
  };
  const stop = async (graceful) => {
    if (graceful) await request("stop");
    record("terminate", { generation, cycle, graceful });
    worker.terminate(); worker = null;
    canvas.remove();
  };
  const started = performance.now();
  try {
    await start();
    for (cycle = 0; cycle < cycles; cycle++) {
      if (mode === "recorded-startup") {
        await request("replay");
        await frame();
        await stop(true); await start();
        if (faults.length) throw Error(JSON.stringify(faults));
        continue;
      }
      if (mode.startsWith("unused-")) {
        await request("acquire-unused");
        await frame();
        if (mode === "unused-replace") { await stop(false); await start(); }
        if (faults.length) throw Error(JSON.stringify(faults));
        continue;
      }
      if ((mode.startsWith("replace") || (mode === "backlog-replace" && cycle % 10 === 0)) && cycle) {
        await request("submit");
        await stop(mode === "replace-graceful");
        await start();
      } else if (mode.startsWith("recover") && cycle) {
        await request("recover", { unconfigure: mode === "recover-unconfigure" });
      }
      const size = (mode.startsWith("resize") || mode === "backlog-resize") && cycle % 2 ? [640, 480] : [800, 600];
      await request("frame", { resize: (mode.startsWith("same-size") || mode.startsWith("resize") || mode === "backlog-resize") ? size : null, reconfigure: mode.startsWith("resize") || mode === "backlog-resize", readback: !mode.startsWith("backlog-"), load: mode.startsWith("backlog-") });
      if (!mode.endsWith("-burst") && !mode.startsWith("backlog-")) await frame();
      if (faults.length) throw Error(JSON.stringify(faults));
      if (cycle % 20 === 0) record("progress", { mode, cycle, generation });
    }
    if (mode.startsWith("backlog-")) await request("frame");
    await stop(true);
    await frame();
    if (faults.length) throw Error(JSON.stringify(faults));
    return { mode, memoryMiB, implementation, pass: true, cycles, generations: generation, ms: performance.now() - started, faults, trace };
  } catch (error) {
    record("failed", { mode, cycle, generation, message: String(error) });
    return { mode, memoryMiB, implementation, pass: false, cyclesCompleted: cycle, generations: generation, error: String(error), faults, trace };
  } finally {
    worker?.terminate(); canvas?.remove();
    for (const p of pending.values()) { clearTimeout(p.timer); p.reject(Error("probe stopped")); }
  }
};
