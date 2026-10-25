// Plain WebGPU, no Alto: present continuously while the GPU is behind, and
// resize the canvas between frames. Mirrors crbug.com/557281760 so its
// controls (main thread, light load, one frame in flight) run the same code.

const SHADER = `
struct Params { iterations: u32, size: vec2f }
@group(0) @binding(0) var<uniform> params: Params;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
  let p = array(vec2f(-1, -3), vec2f(3, 1), vec2f(-1, 1));
  return vec4f(p[i], 0, 1);
}
@fragment fn heavy(@builtin(position) pos: vec4f) -> @location(0) vec4f {
  var v = vec3f(pos.xy / params.size, 0.5);
  for (var i = 0u; i < params.iterations; i++) { v = fract(sin(v * 1.7 + v.yzx) * 43758.5); }
  return vec4f(v, 1);
}
@group(0) @binding(1) var source: texture_2d<f32>;
@fragment fn blit(@builtin(position) pos: vec4f) -> @location(0) vec4f {
  return textureLoad(source, vec2i(pos.xy), 0);
}`;

/** Starts the loop on `canvas` and returns controls. `report` receives lost / validation events. */
export async function start(canvas, options, report) {
  const adapter = await navigator.gpu.requestAdapter();
  const device = await adapter.requestDevice();
  const context = canvas.getContext("webgpu");
  const format = navigator.gpu.getPreferredCanvasFormat();
  context.configure({ device, format, alphaMode: "opaque", usage: GPUTextureUsage.RENDER_ATTACHMENT });
  const t0 = performance.now();
  let lost = null;
  device.lost.then((info) => {
    lost = { reason: info.reason, message: info.message, atMs: Math.round(performance.now() - t0) };
    report({ t: "lost", ...lost });
  });
  device.addEventListener("uncapturederror", (e) => report({ t: "validation", message: e.error.message }));

  const module = device.createShaderModule({ code: SHADER });
  const heavyLayout = device.createBindGroupLayout({ entries: [
    { binding: 0, visibility: GPUShaderStage.FRAGMENT, buffer: {} }] });
  const blitLayout = device.createBindGroupLayout({ entries: [
    { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "unfilterable-float" } }] });
  const heavyPipeline = device.createRenderPipeline({
    layout: device.createPipelineLayout({ bindGroupLayouts: [heavyLayout] }),
    vertex: { module, entryPoint: "vs" },
    fragment: { module, entryPoint: "heavy", targets: [{ format: "rgba8unorm" }] },
  });
  const blitPipeline = device.createRenderPipeline({
    layout: device.createPipelineLayout({ bindGroupLayouts: [blitLayout] }),
    vertex: { module, entryPoint: "vs" },
    fragment: { module, entryPoint: "blit", targets: [{ format }] },
  });
  const params = device.createBuffer({ size: 16, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
  const heavyGroup = device.createBindGroup({ layout: heavyLayout, entries: [{ binding: 0, resource: { buffer: params } }] });

  let iterations = options.light ? 0 : 8;
  let offscreen = null, blitGroup = null;
  function render() {
    const { width, height } = canvas;
    if (!offscreen || offscreen.width !== width || offscreen.height !== height) {
      offscreen?.destroy();
      offscreen = device.createTexture({ size: [width, height], format: "rgba8unorm",
        usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING });
      blitGroup = device.createBindGroup({ layout: blitLayout, entries: [{ binding: 1, resource: offscreen.createView() }] });
    }
    device.queue.writeBuffer(params, 0, new Uint32Array([iterations]));
    device.queue.writeBuffer(params, 8, new Float32Array([width, height]));
    const encoder = device.createCommandEncoder();
    const heavy = encoder.beginRenderPass({ colorAttachments: [{ view: offscreen.createView(), loadOp: "clear", storeOp: "store" }] });
    heavy.setPipeline(heavyPipeline); heavy.setBindGroup(0, heavyGroup); heavy.draw(3); heavy.end();
    const blit = encoder.beginRenderPass({ colorAttachments: [{
      view: context.getCurrentTexture().createView(), loadOp: "clear", storeOp: "store" }] });
    blit.setPipeline(blitPipeline); blit.setBindGroup(0, blitGroup); blit.draw(3); blit.end();
    device.queue.submit([encoder.finish()]);
  }

  // Raise the per-pixel loop until one frame takes longer than ~1.5 vsyncs on this adapter.
  const calibration = [];
  if (!options.light) {
    const timed = async () => {
      const started = performance.now();
      render();
      await device.queue.onSubmittedWorkDone();
      return performance.now() - started;
    };
    // The first frames include pipeline compilation.
    await timed();
    await timed();
    for (;;) {
      const ms = Math.min(await timed(), await timed());
      calibration.push({ iterations, ms: Math.round(ms) });
      if (ms > 25 || iterations >= 8192) break;
      iterations *= 2;
    }
  }

  let pendingSize = null, frames = 0, resizes = 0, skipped = 0, inflight = 0, maxInflight = 0, stopped = false;
  const gpuMs = [];
  const schedule = () => {
    if (stopped || lost) return;
    if (options.loop === "task") setTimeout(frame, 0);
    else requestAnimationFrame(frame);
  };
  function frame() {
    if (stopped || lost) return;
    if (pendingSize) {
      // The reported order: size first, then acquire in the same callback.
      canvas.width = pendingSize[0];
      canvas.height = pendingSize[1];
      pendingSize = null;
      resizes++;
    }
    if (options.inflight && inflight >= options.inflight) {
      skipped++;
      return schedule();
    }
    render();
    frames++;
    inflight++;
    maxInflight = Math.max(maxInflight, inflight);
    const submitted = performance.now();
    device.queue.onSubmittedWorkDone().then(
      () => { inflight--; if (gpuMs.length < 4096) gpuMs.push(performance.now() - submitted); },
      () => { inflight--; });
    schedule();
  }
  schedule();
  return {
    resize(width, height) { pendingSize = [width, height]; },
    stop() {
      stopped = true;
      const sorted = [...gpuMs].sort((a, b) => a - b);
      const at = (q) => (sorted.length ? Math.round(sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))]) : null);
      return { adapter: adapter.info.description || adapter.info.architecture, iterations, calibration,
        frames, resizes, skipped, maxInflight, submitToDoneMs: { p50: at(0.5), p95: at(0.95) }, lost };
    },
  };
}
