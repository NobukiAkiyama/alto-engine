/**
 * A PNG encoder for straight-alpha RGBA bytes. A canvas stores premultiplied pixels,
 * so `convertToBlob` after `putImageData` rounds low-alpha colours twice; this writes
 * the bytes it is given. Output: 8-bit RGBA, no interlace, sRGB chunk, Paeth filter.
 */
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(parts: Uint8Array[]): number {
  let c = 0xffffffff;
  for (const p of parts) for (let i = 0; i < p.length; i++) c = CRC_TABLE[(c ^ p[i]) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(12 + data.length);
  const v = new DataView(out.buffer);
  const name = Uint8Array.from(type, (ch) => ch.charCodeAt(0));
  v.setUint32(0, data.length);
  out.set(name, 4);
  out.set(data, 8);
  v.setUint32(8 + data.length, crc32([name, data]));
  return out;
}

function paeth(a: number, b: number, c: number): number {
  const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

/** `pixels` holds `width * height` straight-alpha RGBA pixels, row by row. */
export async function encodePng(pixels: Uint8Array | Uint8ClampedArray, width: number, height: number): Promise<Blob> {
  if (!(Number.isInteger(width) && Number.isInteger(height) && width > 0 && height > 0))
    throw Error("INVALID_ARGUMENT: PNG size");
  if (pixels.length !== width * height * 4) throw Error("INVALID_ARGUMENT: PNG pixel count");
  const stride = width * 4;
  const filtered = new Uint8Array((stride + 1) * height);
  for (let y = 0; y < height; y++) {
    const row = y * stride, out = y * (stride + 1);
    filtered[out] = 4;
    for (let i = 0; i < stride; i++) {
      const a = i >= 4 ? pixels[row + i - 4] : 0;
      const b = y ? pixels[row - stride + i] : 0;
      const c = y && i >= 4 ? pixels[row - stride + i - 4] : 0;
      filtered[out + 1 + i] = (pixels[row + i] - paeth(a, b, c)) & 255;
    }
  }
  const compressed = new Uint8Array(
    await new Response(new Blob([filtered]).stream().pipeThrough(new CompressionStream("deflate"))).arrayBuffer(),
  );
  const header = new Uint8Array(13);
  const hv = new DataView(header.buffer);
  hv.setUint32(0, width);
  hv.setUint32(4, height);
  header.set([8, 6, 0, 0, 0], 8);
  const signature = Uint8Array.from([137, 80, 78, 71, 13, 10, 26, 10]);
  return new Blob(
    [signature, chunk("IHDR", header), chunk("sRGB", Uint8Array.of(0)), chunk("IDAT", compressed), chunk("IEND", new Uint8Array(0))],
    { type: "image/png" },
  );
}
