// Decode Worker: turns the engine's decode requests into pixels off the engine thread.
// The handler is installed before the module loads, so no request is dropped.
import init, { decodeBatch } from "./pkg/alto_decode.js";
const ready = init().then(() => postMessage({ ready: true }));
self.onmessage = async ({ data }) => {
  await ready;
  const out = decodeBatch(new Uint8Array(data.batch));
  postMessage({ id: data.id, out: out.buffer }, [out.buffer]);
};
