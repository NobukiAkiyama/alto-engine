import assert from "node:assert/strict";
import { test } from "node:test";
import { inflateSync } from "node:zlib";
import { TIFF_STRIP_ROWS, tiffBlob, tiffStrips, tiffStripsJson } from "../dist/tiff.js";

/** The stored bytes of a strip: a view when plain, a Blob when Deflated. */
async function stripBytes(strip) {
  return strip.data instanceof Blob ? new Uint8Array(await strip.data.arrayBuffer()) : strip.data;
}

/** A test-only writer for the header `alto_store::tiff::rgba8_header` produces. */
function referenceHeader(width, height, compression, strips) {
  const tagCount = 12;
  const bitsAt = 8 + 2 + tagCount * 12 + 4;
  const formatAt = bitsAt + 8;
  const tableAt = formatAt + 8;
  const multiple = strips.length > 1;
  const headerLen = tableAt + (multiple ? strips.length * 8 : 0);
  const offsets = [];
  let offset = headerLen;
  for (const s of strips) {
    offsets.push(offset);
    offset += s.bytes;
  }
  const out = new Uint8Array(headerLen);
  const v = new DataView(out.buffer);
  out.set([0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00]);
  v.setUint16(8, tagCount, true);
  // tag, type, count, value (SHORT inline; LONG/SHORT arrays by offset).
  const tags = [
    [256, 4, 1, width],
    [257, 4, 1, height],
    [258, 3, 4, bitsAt],
    [259, 3, 1, compression === "deflate" ? 8 : 1],
    [262, 3, 1, 2],
    [273, 4, strips.length, multiple ? tableAt : headerLen],
    [277, 3, 1, 4],
    [278, 4, 1, TIFF_STRIP_ROWS],
    [279, 4, strips.length, multiple ? tableAt + strips.length * 4 : strips[0].bytes],
    [284, 3, 1, 1],
    [338, 3, 1, 2],
    [339, 3, 4, formatAt],
  ];
  tags.forEach(([tag, type, count, value], i) => {
    const at = 10 + i * 12;
    v.setUint16(at, tag, true);
    v.setUint16(at + 2, type, true);
    v.setUint32(at + 4, count, true);
    if (type === 3 && count === 1) v.setUint16(at + 8, value, true);
    else v.setUint32(at + 8, value, true);
  });
  v.setUint32(10 + tagCount * 12, 0, true);
  [8, 8, 8, 8, 1, 1, 1, 1].forEach((n, i) => v.setUint16(bitsAt + i * 2, n, true));
  if (multiple) {
    strips.forEach((s, i) => v.setUint32(tableAt + i * 4, offsets[i], true));
    strips.forEach((s, i) => v.setUint32(tableAt + strips.length * 4 + i * 4, s.bytes, true));
  }
  return out;
}

/** Reads the tags and strips of a little-endian TIFF written by the header above. */
function parseTiff(bytes) {
  const v = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  assert.equal(v.getUint16(0, true), 0x4949);
  assert.equal(v.getUint16(2, true), 42);
  const ifd = v.getUint32(4, true);
  const count = v.getUint16(ifd, true);
  const tags = new Map();
  for (let i = 0; i < count; i++) {
    const at = ifd + 2 + i * 12;
    const tag = v.getUint16(at, true), type = v.getUint16(at + 2, true);
    const n = v.getUint32(at + 4, true), size = type === 3 ? 2 : 4;
    const valueAt = size * n <= 4 ? at + 8 : v.getUint32(at + 8, true);
    const values = [];
    for (let k = 0; k < n; k++)
      values.push(size === 2 ? v.getUint16(valueAt + k * 2, true) : v.getUint32(valueAt + k * 4, true));
    tags.set(tag, { type, values });
  }
  const width = tags.get(256).values[0], height = tags.get(257).values[0];
  const offsets = tags.get(273).values, counts = tags.get(279).values;
  const rowsPerStrip = tags.get(278).values[0];
  const data = new Uint8Array(width * height * 4);
  let row = 0;
  offsets.forEach((offset, i) => {
    const stored = bytes.subarray(offset, offset + counts[i]);
    const strip = tags.get(259).values[0] === 8 ? inflateSync(stored) : stored;
    data.set(strip, row * width * 4);
    row += Math.min(rowsPerStrip, height - row);
  });
  return { width, height, tags, data };
}

function sample(width, height) {
  const pixels = new Uint8Array(width * height * 4);
  for (let i = 0; i < pixels.length; i++) pixels[i] = (i * 31 + (i >> 4)) & 255;
  return pixels;
}

test("tiffStrips cuts at the tile row and keeps every byte", async () => {
  const width = 3, height = 600, pixels = sample(width, height);
  const strips = await tiffStrips(pixels, width, height, "none");
  assert.deepEqual(strips.map((s) => s.rows), [256, 256, 88]);
  assert.deepEqual(strips.map((s) => s.bytes), [256 * 12, 256 * 12, 88 * 12]);
  assert.deepEqual(Buffer.concat(strips.map((s) => s.data)), Buffer.from(pixels));
});

test("Deflated strips stay Blobs and inflate back to the displayed pixels", async () => {
  const width = 7, height = 300, pixels = sample(width, height);
  const strips = await tiffStrips(pixels, width, height, "deflate");
  assert.deepEqual(strips.map((s) => s.rows), [256, 44]);
  for (const strip of strips) {
    assert.ok(strip.data instanceof Blob);
    assert.equal(strip.bytes, strip.data.size);
  }
  const restored = await Promise.all(strips.map((s) => s.data.arrayBuffer()));
  assert.deepEqual(Buffer.concat(restored.map((b) => inflateSync(Buffer.from(b)))), Buffer.from(pixels));
});

test("tiffStripsJson carries only the rows and byte counts", async () => {
  const strips = await tiffStrips(sample(2, 257), 2, 257, "none");
  assert.equal(tiffStripsJson(strips), '[{"rows":256,"bytes":2048},{"rows":1,"bytes":8}]');
});

test("tiffBlob writes the header before the strips", async () => {
  const strips = await tiffStrips(sample(2, 2), 2, 2, "none");
  const blob = tiffBlob(Uint8Array.of(1, 2, 3), strips);
  assert.equal(blob.type, "image/tiff");
  assert.deepEqual(new Uint8Array(await blob.arrayBuffer()), Uint8Array.of(1, 2, 3, ...strips[0].data));
});

test("uncompressed and Deflated TIFF pixels match the display pixels", async () => {
  for (const [width, height] of [[5, 100], [5, 400]]) {
    const pixels = sample(width, height);
    for (const compression of ["none", "deflate"]) {
      const strips = await tiffStrips(pixels, width, height, compression);
      const header = referenceHeader(width, height, compression, strips);
      const blob = tiffBlob(header, strips);
      assert.equal(blob.size, header.length + strips.reduce((size, strip) => size + strip.bytes, 0));
      const parsed = parseTiff(new Uint8Array(await blob.arrayBuffer()));
      assert.equal(parsed.width, width);
      assert.equal(parsed.height, height);
      assert.deepEqual([...parsed.tags.keys()], [256, 257, 258, 259, 262, 273, 277, 278, 279, 284, 338, 339]);
      assert.deepEqual(parsed.tags.get(258).values, [8, 8, 8, 8]);
      assert.deepEqual(parsed.tags.get(278).values, [TIFF_STRIP_ROWS]);
      assert.deepEqual(parsed.tags.get(338).values, [2]);
      assert.deepEqual(parsed.tags.get(339).values, [1, 1, 1, 1]);
      assert.deepEqual(parsed.tags.get(259).values, [compression === "deflate" ? 8 : 1]);
      assert.deepEqual(parsed.data, pixels);
    }
  }
});

test("tiffStrips refuses a size that does not match the pixels", async () => {
  await assert.rejects(tiffStrips(new Uint8Array(16), 3, 1, "none"), /INVALID_ARGUMENT/);
  await assert.rejects(tiffStrips(new Uint8Array(16), 0, 0, "none"), /INVALID_ARGUMENT/);
  await assert.rejects(tiffStrips(new Uint8Array(16), 2, 2, "lzw"), /INVALID_ARGUMENT/);
});
