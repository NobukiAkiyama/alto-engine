// The v3 mask references in JavaScript, for `cpu_composite::mask_v3_rust_and_js_references_agree`.
// argv[2] is a JSON list of mask transforms from the Rust test, so both sides
// sample with the same f64 matrices. Output: little-endian u16 channels for modes
// 21, 22 and 23 over the grids below, then one byte per pixel for each transform,
// then the feathered selection bytes.
import { featheredRectangle, maskAdjustQ14, maskByteV3, maskLayerQ14, maskSelectQ14 } from "./cpu-composite.js";

const E = [0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384].map(BigInt);
const pixels = E.flatMap((a) => E.filter((v) => v <= a).map((v) => [v, v, v, a]));
const byteQ14 = (b) => BigInt(Math.trunc((b * 16384 + 127) / 255));
const all = Array.from({ length: 256 }, (_, b) => byteQ14(b));
const some = [0, 1, 2, 127, 128, 129, 253, 254, 255].map(byteQ14);
const few = [0, 1, 127, 128, 254, 255].map(byteQ14);
const opacities = [0, 1, 8192, 16383, 16384].map(BigInt);

const chunks = [];
const u16 = (values) => chunks.push(Buffer.from(new Uint16Array(values.map(Number)).buffer));
for (const v of pixels) for (const m of all) for (const o of E) u16(maskLayerQ14(v, m * o));
for (const input of pixels) for (const filtered of pixels) for (const s of some) u16(maskSelectQ14(input, filtered, s));
for (const orig of pixels) for (const filt of pixels) for (const m of few) for (const o of opacities) {
  for (let blend = 0; blend < 4; blend++) u16(maskAdjustQ14(orig, filt, m * o, blend));
}
const pattern = (x, y) => (x >= 0 && y >= 0 && x < 256 && y < 256 ? (x * 7 + y * 13) & 255 : 0);
for (const transform of JSON.parse(process.argv[2])) {
  const bytes = new Uint8Array(256 * 256);
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) bytes[y * 256 + x] = maskByteV3(pattern, transform, x, y);
  chunks.push(Buffer.from(bytes));
}
chunks.push(Buffer.from(featheredRectangle([40.25, 40.75, 216.5, 216.0], 64)));
process.stdout.write(Buffer.concat(chunks));
