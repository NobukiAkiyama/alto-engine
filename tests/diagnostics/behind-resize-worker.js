import { start } from "./behind-resize-render.js";

let loop = null;
self.onmessage = async ({ data: m }) => {
  try {
    if (m.t === "start") {
      loop = await start(m.canvas, m.options, (event) => postMessage(event));
      postMessage({ t: "started" });
    } else if (m.t === "resize") loop?.resize(m.width, m.height);
    else if (m.t === "stop") postMessage({ t: "stats", value: loop?.stop() ?? null });
  } catch (error) {
    postMessage({ t: "error", message: String(error?.stack || error) });
  }
};
