// Independent JavaScript result stream for the Rust/JS v3 transform comparison.
import { FIXED_IDENTITY, fixedAffine, fixedConcat, fixedInverse } from "./fixed-affine.js";

const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const chains = JSON.parse(Buffer.concat(chunks).toString());
const lines = chains.map((chain) => {
  let world = FIXED_IDENTITY;
  for (const values of chain) {
    const local = fixedAffine(values);
    if (!local) return "X";
    world = fixedConcat(world, local);
    if (!world) return "X";
  }
  const inverse = fixedInverse(world);
  if (inverse === undefined) return "X";
  if (inverse === null) return "S";
  return [...inverse.m, inverse.tx, inverse.ty].join(",");
});
process.stdout.write(lines.join("\n") + "\n");
