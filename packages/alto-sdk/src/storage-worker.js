// Storage Worker: runs the OPFS journal (see storage-core.js) off the engine thread.
import { handle } from "./storage-core.js";
let chain = Promise.resolve();
self.onmessage = ({ data }) => {
  chain = chain.then(async () => {
    try {
      postMessage({ id: data.id, ok: true, value: await handle(data) });
    } catch (e) {
      postMessage({ id: data.id, ok: false, error: String(e) });
    }
  });
};
