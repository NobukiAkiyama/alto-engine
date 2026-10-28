// The v3 mask stages away from the origin in JavaScript, for
// `cpu_composite::mask_origin_rust_and_js_references_agree`. argv[2] is a JSON
// list of `[region, transform]` from the Rust test (`mask_origin_cases`), where
// region is `[x, y, level scale]`. Output: for each case, the RGBA8 tiles of
// `renderM5V3` for a masked layer at opacity 0.5 (mode 21), a selection-limited
// color-adjust (mode 22) and a masked screen adjustment layer at opacity 0.5 over
// an opaque backdrop (mode 23), the `origin ...` programs of `mask_origin_programs`.
import { colorAdjustGradient, renderM5V3 } from "./cpu-composite.js";

const gradient = colorAdjustGradient();
const opaque = { ...gradient, pixels: gradient.pixels.map((v, i) => (i % 4 === 3 ? 255 : v)) };
const pattern = new Uint8Array(256 * 256);
for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) pattern[y * 256 + x] = (x * 7 + y * 13) & 255;
const vivid = { contrast: 1.7, saturation: 1.6, brightness: 0 };

const chunks = [];
for (const [[x, y, scale], transform] of JSON.parse(process.argv[2])) {
  const mask = { bytes: pattern, transform };
  const render = (layers) => chunks.push(Buffer.from(renderM5V3(layers, 256, 256, [x, y], scale)));
  render([{ image: gradient, opacity: 0.5, mask }]);
  render([{ image: gradient, effects: [{ ...vivid, selection: mask }] }]);
  render([{ image: opaque }, { adjust: vivid, opacity: 0.5, mode: 2, mask }]);
}
process.stdout.write(Buffer.concat(chunks));
