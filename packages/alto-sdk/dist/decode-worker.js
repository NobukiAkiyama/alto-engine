// Decode Worker: turns the engine's decode requests into pixels off the engine thread.
// The handler is installed before the module loads, so no request is dropped. Every
// answer reports this Worker's WASM memory so the engine can count the whole pool.
import init, { decodeBatch } from "./pkg/alto_decode.js";
let memory = null;
let stalled = false;
const ready = init().then((wasm) => {
  memory = wasm.memory;
  postMessage({ ready: true, memory: memory.buffer.byteLength });
});
self.onmessage = async ({ data }) => {
  await ready;
  // Fault injection for the engine's timeout test: keep requests, never answer.
  if (data.stall) stalled = true;
  if (stalled) return;
  const out = decodeBatch(new Uint8Array(data.batch));
  postMessage({ id: data.id, out: out.buffer, memory: memory.buffer.byteLength }, [out.buffer]);
};
