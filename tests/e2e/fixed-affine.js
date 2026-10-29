// Independent BigInt reference for the version 3 host transform contract.
// The stored f64 is only used to read its IEEE-754 bits.
const Q32 = 1n << 32n;
const LINEAR_LIMIT = 1n << 46n;
const TRANSLATION_LIMIT = 1n << 47n;
const I32_MIN_PLUS_ONE = -(1n << 31n) + 1n;
const I32_MAX = (1n << 31n) - 1n;
const TRANSLATION_INVERSE_LIMIT = 1n << 53n;
const bitsBuffer = new ArrayBuffer(8);
const bitsView = new DataView(bitsBuffer);

export function f64Bits(value) {
  bitsView.setFloat64(0, value, true);
  return bitsView.getBigUint64(0, true);
}

const abs = (n) => n < 0n ? -n : n;
function inRange(v, limit) { return abs(v) < limit; }

export function qFromF64Bits(bits, fractionalBits) {
  const exponent = (bits >> 52n) & 0x7ffn;
  if (exponent === 0x7ffn) return null;
  const fraction = bits & ((1n << 52n) - 1n);
  const mantissa = exponent === 0n ? fraction : fraction | (1n << 52n);
  const power = exponent === 0n ? -1074n : exponent - 1075n;
  const shift = power + BigInt(fractionalBits);
  let magnitude;
  if (shift >= 0n) magnitude = mantissa << shift;
  else {
    const right = -shift;
    magnitude = right >= 54n ? 0n : (mantissa + (1n << (right - 1n))) >> right;
  }
  const value = bits >> 63n === 0n ? magnitude : -magnitude;
  return inRange(value, 1n << 63n) || value === -(1n << 63n) ? value : null;
}

export function fixedAffine(values) {
  const m = values.slice(0, 4).map((v) => qFromF64Bits(f64Bits(v), 32));
  const t = values.slice(4, 6).map((v) => qFromF64Bits(f64Bits(v), 16));
  if (m.some((v) => v === null || !inRange(v, LINEAR_LIMIT)) ||
      t.some((v) => v === null || !inRange(v, TRANSLATION_LIMIT))) return null;
  return { m, t };
}

export const FIXED_IDENTITY = { m: [Q32, 0n, 0n, Q32], t: [0n, 0n] };

/** Round an exact rational half away from zero. */
export function fixedRoundDiv(n, d) {
  if (d === 0n) throw Error("zero denominator");
  const an = abs(n), ad = abs(d);
  const q = an / ad, r = an % ad;
  const rounded = q + (2n * r >= ad ? 1n : 0n);
  return (n < 0n) !== (d < 0n) ? -rounded : rounded;
}

export function fixedConcat(p, n) {
  const [a, b, c, d] = p.m, [e, f, g, h] = n.m;
  const m = [
    fixedRoundDiv(a*e + c*f, Q32), fixedRoundDiv(b*e + d*f, Q32),
    fixedRoundDiv(a*g + c*h, Q32), fixedRoundDiv(b*g + d*h, Q32),
  ];
  const t = [
    fixedRoundDiv(a*n.t[0] + c*n.t[1] + p.t[0]*Q32, Q32),
    fixedRoundDiv(b*n.t[0] + d*n.t[1] + p.t[1]*Q32, Q32),
  ];
  return m.every((v) => inRange(v, LINEAR_LIMIT)) && t.every((v) => inRange(v, TRANSLATION_LIMIT))
    ? { m, t } : null;
}

/** `undefined` means out of sampler range; `null` means exactly singular. */
export function fixedInverse(world) {
  const [a, b, c, d] = world.m, [tx, ty] = world.t;
  const det = a*d - b*c;
  if (det === 0n) return null;
  const m = [d, -b, -c, a].map((v) => fixedRoundDiv(v * (1n << 48n), det));
  const t = [
    fixedRoundDiv((c*ty - d*tx) * Q32, det),
    fixedRoundDiv((b*tx - a*ty) * Q32, det),
  ];
  if (m.some((v) => v < I32_MIN_PLUS_ONE || v > I32_MAX) ||
      t.some((v) => !inRange(v, TRANSLATION_INVERSE_LIMIT))) return undefined;
  return { m: m.map(Number), tx: t[0], ty: t[1] };
}
