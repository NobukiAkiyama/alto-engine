// Deliberately independent of Alto, Rust, WASM, OPFS and decode Workers.
let canvas, context, adapter, device, buffer;
let fault = null;
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
function configure() {
  context.configure({ device, format: navigator.gpu.getPreferredCanvasFormat(),
    usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC, alphaMode: "opaque" });
}
function encode(readback) {
  if (fault) throw Error(fault);
  const texture = context.getCurrentTexture();
  const encoder = device.createCommandEncoder();
  const pass = encoder.beginRenderPass({ label: "raw-present", colorAttachments: [{
    view: texture.createView(), loadOp: "clear", storeOp: "store", clearValue: [0, 1, 0, 1],
  }] });
  pass.end();
  if (readback) encoder.copyTextureToBuffer({ texture }, { buffer, bytesPerRow: 256 }, [1, 1]);
  device.queue.submit([encoder.finish()]);
}
async function execute(m) {
  if (m.op === "init") {
    canvas = m.canvas;
    context = canvas.getContext("webgpu");
    device = await newDevice();
    configure();
    buffer = device.createBuffer({ size: 256, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const { vendor, architecture, device: name, description } = adapter.info;
    report("adapter", { vendor, architecture, device: name, description });
  } else if (m.op === "frame") {
    if (m.resize) {
      canvas.width = m.resize[0]; canvas.height = m.resize[1];
      if (m.reconfigure) {
        // wgpu::Surface::configure also writes the two dimensions.
        canvas.width = m.resize[0]; canvas.height = m.resize[1];
        configure();
      }
    }
    encode(true);
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
    buffer.destroy();
    if (m.unconfigure) context.unconfigure();
    device = await newDevice();
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
