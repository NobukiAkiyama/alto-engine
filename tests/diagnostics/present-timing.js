// Injected by verify-browser.mjs only with ALTO_BROWSER_PRESENT_TIMING=1.
// Durations measure CPU encoding and submit-to-queue-completion latency, not GPU
// timestamp queries. The latter includes compilation and older queued work.
(() => {
  const realm = crypto.randomUUID();
  const commands = new WeakMap(), destroyed = new WeakSet();
  let frameId = 0, tickStart = 0, tickMs = 0, submits = [], serial = 0;
  const emit = (kind, fields = {}) => console.debug("PRESENT-TIMING " + JSON.stringify({ realm, at: performance.now(), kind, ...fields }));
  const method = (proto, name, wrap) => {
    const original = proto?.[name];
    if (original) proto[name] = wrap(original);
  };
  method(GPUDevice.prototype, "destroy", (original) => function (...args) {
    destroyed.add(this);
    emit("destroy-device");
    return original.apply(this, args);
  });
  method(GPUAdapter.prototype, "requestDevice", (original) => function (...args) {
    return original.apply(this, args).then((device) => {
      device.lost.then(({ reason, message }) => emit("device-lost", { reason, message, intentional: destroyed.has(device) }));
      device.addEventListener("uncapturederror", ({ error }) => emit("validation", { message: error.message }));
      return device;
    });
  });
  for (const name of ["createComputePipeline", "createRenderPipeline"]) {
    method(GPUDevice.prototype, name, (original) => function (desc) {
      const start = performance.now();
      const pipeline = original.call(this, desc);
      emit("pipeline", { label: desc.label, cpuMs: performance.now() - start });
      // Fence prior creation commands without submitting additional GPU work.
      this.queue.onSubmittedWorkDone().then(() => emit("pipeline-ready", { label: desc.label, ms: performance.now() - start }),
        (error) => emit("queue-error", { message: String(error) }));
      return pipeline;
    });
  }
  for (const name of ["beginComputePass", "beginRenderPass"]) {
    method(GPUCommandEncoder.prototype, name, (original) => function (...args) {
      const begin = performance.now();
      const pass = original.apply(this, args), stages = commands.get(this) || [];
      commands.set(this, stages);
      const stage = { kind: name, label: args[0]?.label || "", dispatches: 0, draws: 0, cpuMs: performance.now() - begin };
      const end = pass.end.bind(pass);
      pass.end = () => { const result = end(); stage.cpuMs = performance.now() - begin; return result; };
      stages.push(stage);
      const set = pass.setPipeline.bind(pass);
      pass.setPipeline = (pipeline) => { stage.pipeline = pipeline.label; return set(pipeline); };
      for (const draw of ["dispatchWorkgroups", "draw", "drawIndexed"]) {
        if (!pass[draw]) continue;
        const call = pass[draw].bind(pass);
        pass[draw] = (...values) => { stage[draw === "dispatchWorkgroups" ? "dispatches" : "draws"]++; return call(...values); };
      }
      return pass;
    });
  }
  method(GPUCommandEncoder.prototype, "finish", (original) => function (...args) {
    const buffer = original.apply(this, args);
    commands.set(buffer, commands.get(this) || []);
    return buffer;
  });
  method(GPUQueue.prototype, "submit", (original) => function (buffers) {
    const list = Array.from(buffers), start = performance.now(), id = ++serial, frame = frameId;
    const stages = list.flatMap((buffer) => commands.get(buffer) || []);
    const result = original.call(this, list);
    submits.push({ id, stages, cpuMs: performance.now() - start });
    this.onSubmittedWorkDone().then(() => emit("submit-done", { id, frame, stages, ms: performance.now() - start }),
      (error) => emit("queue-error", { id, message: String(error) }));
    return result;
  });
  globalThis.altoPresentTiming = {
    emit,
    mark(label) { emit("mark", { label }); return realm; },
    begin() { frameId++; tickStart = performance.now(); submits = []; },
    end() { tickMs = performance.now() - tickStart; },
    frame(frame, where, streak, idle) {
      emit("frame", { frame: frameId, where, streakBefore: streak, idle, streakAfter: idle ? streak + 1 : 0,
        tickMs, pending: frame.pending, composed: frame.composed, deferred: frame.presentDeferred,
        profile: frame.profile, display: frame.display, submits });
    },
  };
})();
