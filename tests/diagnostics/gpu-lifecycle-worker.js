// The default probe is raw WebGPU. The optional WASM probe adds only Runtime,
// keeping the SDK scheduler, OPFS and decode Workers outside the experiment.
let canvas, context, adapter, device, buffer, memoryMiB = 0, allocations = [];
let fault = null, runtime = null, loadPipeline = null, loadGroup = null;
const report = (event, detail = {}) => postMessage({ event, at: Date.now(), ...detail });
async function newDevice() {
  adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw Error("no WebGPU adapter");
  const next = await adapter.requestDevice();
  const state = { expected: false };
  next.addEventListener("uncapturederror", ({ error }) => {
    fault = String(error.message);
    report("validation", { message: fault });
  });
  next.lost.then(({ reason, message }) => {
    if (!state.expected) fault = `device lost: ${reason}: ${message}`;
    report("lost", { reason, message, expected: state.expected });
  });
  next.expectedState = state;
  return next;
}
function allocate() {
  allocations = memoryMiB ? [256, 256, 256].map((depthOrArrayLayers) => device.createTexture({
    label: "Alto-sized texture allocation", size: [256, 256, depthOrArrayLayers], format: "rgba8unorm",
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST | GPUTextureUsage.COPY_SRC | GPUTextureUsage.STORAGE_BINDING,
  })) : [];
}
function configure() {
  context.configure({ device, format: navigator.gpu.getPreferredCanvasFormat(),
    usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC, alphaMode: "opaque" });
}
function encode(readback, load = false) {
  if (fault) throw Error(fault);
  const texture = context.getCurrentTexture();
  const encoder = device.createCommandEncoder();
  if (load && loadPipeline) {
    const compute = encoder.beginComputePass({ label: "bounded queue backlog" });
    compute.setPipeline(loadPipeline); compute.setBindGroup(0, loadGroup);
    compute.dispatchWorkgroups(256); compute.end();
  }
  const pass = encoder.beginRenderPass({ label: "raw-present", colorAttachments: [{
    view: texture.createView(), loadOp: "clear", storeOp: "store", clearValue: [0, 1, 0, 1],
  }] });
  pass.end();
  if (readback) encoder.copyTextureToBuffer({ texture }, { buffer, bytesPerRow: 256 }, [1, 1]);
  device.queue.submit([encoder.finish()]);
}
async function execute(m) {
  if (m.op === "init" && m.implementation === "wasm") {
    canvas = m.canvas;
    report("init-phase", { phase: "module-import" });
    const { default: init, Runtime } = await import("../../packages/alto-sdk/dist/pkg/alto_wasm.js");
    report("init-phase", { phase: "wasm-load" });
    await init();
    report("init-phase", { phase: "runtime-create" });
    runtime = await Runtime.create(canvas, 512);
    const info = (await navigator.gpu.requestAdapter({ powerPreference: "high-performance" }))?.info;
    report("adapter", { architecture: info?.architecture || "", description: info?.description || "", runtimeName: runtime.adapterName() });
    return;
  }
  if (runtime) {
    if (m.op === "frame") {
      if (m.resize) runtime.resize(...m.resize);
      await runtime.validateFrame();
      const bytes = await runtime.readTile(0, 0);
      if (bytes.length !== 256 * 256 * 4 || bytes.some((b) => b !== 0)) throw Error("empty runtime content pixel differs");
    } else if (m.op === "submit") runtime.tick(8);
    else if (m.op === "recover") {
      runtime.destroyGpu();
      if (m.unconfigure) canvas.getContext("webgpu").unconfigure();
      await runtime.recoverGpu();
    } else if (m.op === "stop") {
      await runtime.gpuIdle();
      canvas.getContext("webgpu").unconfigure();
      runtime.destroyGpu();
      runtime.free(); runtime = null;
    } else throw Error("unknown runtime op " + m.op);
    return;
  }
  if (m.op === "init") {
    canvas = m.canvas;
    memoryMiB = m.memoryMiB;
    context = canvas.getContext("webgpu");
    device = await newDevice();
    allocate();
    configure();
    if (m.backlog) {
      const shader = device.createShaderModule({ code: `
        @group(0) @binding(0) var<storage, read_write> values: array<vec4f>;
        @compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3u) {
          var value = values[id.x] + vec4f(f32(id.x) * .001);
          for (var i = 0u; i < 64u; i++) { value = sin(value + vec4f(.01)); }
          values[id.x] = value;
        }` });
      loadPipeline = device.createComputePipeline({ layout: "auto", compute: { module: shader, entryPoint: "main" } });
      const values = device.createBuffer({ size: 16384 * 16, usage: GPUBufferUsage.STORAGE });
      loadGroup = device.createBindGroup({ layout: loadPipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: { buffer: values } }] });
    }
    buffer = device.createBuffer({ size: 256, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const { vendor, architecture, device: name, description } = adapter.info;
    report("adapter", { vendor, architecture, device: name, description });
  } else if (m.op === "acquire-unused") {
    // Exercise Chromium's clear-on-present path, which a rendered green frame
    // never reaches. Do not initialize the swapchain texture in this control.
    context.getCurrentTexture();
  } else if (m.op === "frame") {
    if (m.resize) {
      canvas.width = m.resize[0]; canvas.height = m.resize[1];
      if (m.reconfigure) {
        // wgpu::Surface::configure also writes the two dimensions.
        canvas.width = m.resize[0]; canvas.height = m.resize[1];
        configure();
      }
    }
    encode(m.readback !== false, m.load);
    if (m.readback === false) return { submitted: true };
    await buffer.mapAsync(GPUMapMode.READ);
    const pixel = Array.from(new Uint8Array(buffer.getMappedRange(), 0, 4));
    buffer.unmap();
    if (pixel.join() !== "0,255,0,255") throw Error("unexpected surface pixel " + pixel);
    if (fault) throw Error(fault);
    return { pixel };
  } else if (m.op === "submit") {
    encode(false); // Reply without waiting for GPU completion, then terminate from the host.
  } else if (m.op === "recover") {
    device.expectedState.expected = true;
    device.destroy();
    for (const texture of allocations) texture.destroy();
    buffer.destroy();
    if (m.unconfigure) context.unconfigure();
    device = await newDevice();
    allocate();
    configure();
    buffer = device.createBuffer({ size: 256, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
  } else if (m.op === "stop") {
    await device.queue.onSubmittedWorkDone();
    context.unconfigure();
    buffer.destroy();
    device.expectedState.expected = true;
    device.destroy();
  } else throw Error("unknown op " + m.op);
}
let tail = Promise.resolve();
onmessage = ({ data }) => {
  tail = tail.then(async () => {
    try { postMessage({ id: data.id, ok: true, value: await execute(data) }); }
    catch (error) { report("failure", { message: String(error) }); postMessage({ id: data.id, ok: false, error: String(error) }); }
  });
};
