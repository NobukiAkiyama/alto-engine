import assert from "node:assert/strict";
import { test } from "node:test";
import { inflateSync } from "node:zlib";
import { encodePng } from "../dist/png.js";

/** Chunks of a PNG, with each CRC checked. */
function chunks(bytes) {
  const v = new DataView(bytes.buffer, bytes.byteOffset), out = [];
  assert.deepEqual([...bytes.subarray(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10]);
  for (let at = 8; at < bytes.length;) {
    const len = v.getUint32(at), type = String.fromCharCode(...bytes.subarray(at + 4, at + 8));
    out.push({ type, data: bytes.subarray(at + 8, at + 8 + len), crc: v.getUint32(at + 8 + len) });
    at += 12 + len;
  }
  return out;
}

function crc32(bytes) {
  let c = 0xffffffff;
  for (const b of bytes) {
    c ^= b;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  }
  return (c ^ 0xffffffff) >>> 0;
}

function unfilter(raw, width, height) {
  const stride = width * 4, out = new Uint8Array(stride * height);
  for (let y = 0; y < height; y++) {
    const f = raw[y * (stride + 1)];
    for (let i = 0; i < stride; i++) {
      const a = i >= 4 ? out[y * stride + i - 4] : 0, b = y ? out[(y - 1) * stride + i] : 0;
      const c = y && i >= 4 ? out[(y - 1) * stride + i - 4] : 0;
      const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
      const pred = [0, a, b, (a + b) >> 1, pa <= pb && pa <= pc ? a : pb <= pc ? b : c][f];
      out[y * stride + i] = (raw[y * (stride + 1) + 1 + i] + pred) & 255;
    }
  }
  return out;
}

test("encodePng writes the given straight RGBA bytes losslessly", async () => {
  const width = 37, height = 11, pixels = new Uint8Array(width * height * 4);
  for (let i = 0; i < pixels.length; i++) pixels[i] = (i * 97 + (i >> 5)) & 255;
  const bytes = new Uint8Array(await (await encodePng(pixels, width, height)).arrayBuffer());
  const list = chunks(bytes);
  assert.deepEqual(list.map((c) => c.type), ["IHDR", "sRGB", "IDAT", "IEND"]);
  for (const c of list) assert.equal(c.crc, crc32([...Buffer.from(c.type, "latin1"), ...c.data]), c.type + " CRC");
  const ihdr = new DataView(list[0].data.buffer, list[0].data.byteOffset);
  assert.equal(ihdr.getUint32(0), width);
  assert.equal(ihdr.getUint32(4), height);
  assert.deepEqual([...list[0].data.subarray(8)], [8, 6, 0, 0, 0]);
  assert.deepEqual(unfilter(inflateSync(list[2].data), width, height), pixels);
});

test("encodePng refuses a size that does not match the pixels", async () => {
  await assert.rejects(encodePng(new Uint8Array(16), 3, 1), /INVALID_ARGUMENT/);
  await assert.rejects(encodePng(new Uint8Array(0), 0, 0), /INVALID_ARGUMENT/);
});
