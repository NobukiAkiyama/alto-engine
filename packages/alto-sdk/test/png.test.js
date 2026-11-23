import assert from "node:assert/strict";
import { test } from "node:test";
import { inflateSync } from "node:zlib";
import { encodePng, encodePngStream } from "../dist/png.js";

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

test("banded PNG preserves Paeth across band boundaries and splits compressed data", async () => {
  const width = 640;
  const height = 520;
  const stride = width * 4;
  const pixels = new Uint8ClampedArray(stride * height);
  let random = 0x12345678;
  for (let i = 0; i < pixels.length; i++) {
    random ^= random << 13;
    random ^= random >>> 17;
    random ^= random << 5;
    pixels[i] = random >>> 24;
  }
  async function* bands() {
    const scratch = new Uint8ClampedArray(stride * 129);
    for (let y = 0; y < height; y += 129) {
      const band = scratch.subarray(0, stride * Math.min(129, height - y));
      band.set(pixels.subarray(y * stride, y * stride + band.length));
      yield band;
    }
  }
  const bytes = new Uint8Array(await new Response(encodePngStream(bands(), width, height)).arrayBuffer());
  const list = chunks(bytes);
  assert.deepEqual(list.slice(0, 2).map((c) => c.type), ["IHDR", "sRGB"]);
  assert.equal(list.at(-1).type, "IEND");
  const idat = list.slice(2, -1);
  assert.ok(idat.length > 1);
  assert.ok(idat.every((c) => c.type === "IDAT" && c.data.length > 0 && c.data.length <= 1024 * 1024));
  for (const c of list) assert.equal(c.crc, crc32([...Buffer.from(c.type, "latin1"), ...c.data]), c.type + " CRC");
  const single = chunks(new Uint8Array(await (await encodePng(pixels, width, height)).arrayBuffer()));
  const filtered = inflateSync(Buffer.concat(idat.map((c) => c.data)));
  assert.deepEqual(filtered, inflateSync(single[2].data));
  assert.deepEqual(unfilter(filtered, width, height), new Uint8Array(pixels));
});

test("banded PNG rejects missing, extra, partial, and oversized bands", async () => {
  for (const bands of [
    [new Uint8Array(8)],
    [new Uint8Array(16), new Uint8Array(8)],
    [new Uint8Array(7)],
    [new Uint8Array(0)],
    [new Uint16Array(16)],
    [null],
  ]) {
    await assert.rejects(new Response(encodePngStream(bands, 2, 2)).arrayBuffer(), /INVALID_ARGUMENT/);
  }
  await assert.rejects(new Response(encodePngStream([new Uint8Array(1028)], 1, 257)).arrayBuffer(), /PNG band size/);
});

test("cancelling a banded PNG closes the input iterator", async () => {
  let closed = false;
  let pulls = 0;
  const pixels = new Uint8Array(256 * 256 * 4);
  let random = 33;
  for (let i = 0; i < pixels.length; i++) {
    random = (Math.imul(random, 1664525) + 1013904223) | 0;
    pixels[i] = random >>> 24;
  }
  async function* bands() {
    try {
      while (pulls < 256) {
        pulls++;
        yield pixels;
      }
    } finally {
      closed = true;
    }
  }
  const reader = encodePngStream(bands(), 256, 65536).getReader();
  await reader.read(); // PNG header
  assert.equal(pulls, 0);
  await reader.read(); // First compressed data
  const afterFirstChunk = pulls;
  await reader.read(); // Buffered by the same write
  assert.equal(pulls, afterFirstChunk);
  await reader.cancel();
  assert.equal(closed, true);
  assert.ok(pulls > 0 && pulls < 256);
});

test("cancelling while the producer is waiting does not wait for the band", { timeout: 2000 }, async () => {
  const entering = Promise.withResolvers();
  const source = Promise.withResolvers();
  const closed = Promise.withResolvers();
  async function* bands() {
    try {
      entering.resolve();
      yield await source.promise;
    } finally {
      closed.resolve();
    }
  }
  const reader = encodePngStream(bands(), 1, 1).getReader();
  await reader.read(); // PNG header
  const pending = reader.read();
  await entering.promise;
  try {
    await reader.cancel();
    assert.equal((await pending).done, true);
  } finally {
    source.resolve(new Uint8Array(4));
  }
  await closed.promise;
});

test("a source error is preserved after partial PNG output", async () => {
  const failure = Error("band read failed");
  async function* bands() {
    yield new Uint8Array(256 * 256 * 4);
    throw failure;
  }
  const reader = encodePngStream(bands(), 256, 512).getReader();
  assert.equal((await reader.read()).done, false);
  await assert.rejects(async () => {
    while (!(await reader.read()).done) { /* drain compressed data */ }
  }, (error) => error === failure);
});
