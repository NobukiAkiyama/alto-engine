import assert from "node:assert/strict";
import { test } from "node:test";
import { FIXED_IDENTITY, f64Bits, qFromF64Bits, fixedAffine, fixedConcat, fixedInverse } from "../../../tests/e2e/fixed-affine.js";

test("v3 fixed affine rounds bits and full translations", () => {
  const tie16 = 2 ** -17, below16 = tie16 - 2 ** -70;
  assert.equal(qFromF64Bits(f64Bits(below16), 16), 0n);
  assert.equal(qFromF64Bits(f64Bits(tie16), 16), 1n);
  assert.equal(qFromF64Bits(f64Bits(-tie16), 16), -1n);
  const tie32 = 2 ** -33;
  assert.equal(qFromF64Bits(f64Bits(tie32), 32), 1n);
  assert.equal(qFromF64Bits(f64Bits(-tie32), 32), -1n);
  assert.equal(qFromF64Bits(f64Bits(Number.NaN), 32), null);
  assert.equal(fixedAffine([16384, 0, 0, 1, 0, 0]), null);
  const parent = { m: [1n << 32n, 0n, -(1n << 31n), 1n << 32n], t: [1n, 0n] };
  const child = { m: FIXED_IDENTITY.m, t: [0n, 1n] };
  assert.equal(fixedConcat(parent, child).t[0], 1n);
});

test("v3 inverse is rounded from one rational division", () => {
  assert.deepEqual(fixedInverse(FIXED_IDENTITY), { m: [65536, 0, 0, 65536], tx: 0n, ty: 0n });
  const rotation = fixedAffine([0, 1, -1, 0, 3, -2]);
  assert.deepEqual(fixedInverse(rotation), { m: [0, -65536, 65536, 0], tx: 131072n, ty: 196608n });
  assert.equal(fixedInverse(fixedAffine([1, 2, 2, 4, 0, 0])), null);
});
