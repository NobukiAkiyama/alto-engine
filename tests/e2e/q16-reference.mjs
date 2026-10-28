// The Q16 rounding in JavaScript, for `cpu_composite::q16_rust_and_js_references_agree`.
// argv[2] is a JSON list of f64 bit patterns (16 hex digits) from the Rust test.
// Output: one line per value, "<version 3 Q16> <version 2 Q16>".
import { quantizeQ16, quantizeQ16V2 } from "./cpu-composite.js";

const view = new DataView(new ArrayBuffer(8));
const lines = [];
for (const hex of JSON.parse(process.argv[2])) {
  view.setBigUint64(0, BigInt(`0x${hex}`));
  const v = view.getFloat64(0);
  lines.push(`${quantizeQ16(v)} ${quantizeQ16V2(v)}`);
}
process.stdout.write(`${lines.join("\n")}\n`);
