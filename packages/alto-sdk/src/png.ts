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

type Rgba = Uint8Array | Uint8ClampedArray;
const BAND_ROWS = 256;
const MAX_IDAT_BYTES = 1024 * 1024;

function checkSize(width: number, height: number, limit: number): void {
  if (!Number.isInteger(width) || !Number.isInteger(height) ||
      width < 1 || width > limit || height < 1 || height > limit) {
    throw Error("INVALID_ARGUMENT: PNG size");
  }
}

function pngHeader(width: number, height: number): Uint8Array<ArrayBuffer> {
  const ihdr = new Uint8Array(13);
  const view = new DataView(ihdr.buffer);
  view.setUint32(0, width);
  view.setUint32(4, height);
  ihdr.set([8, 6, 0, 0, 0], 8);
  return Uint8Array.of(
    137, 80, 78, 71, 13, 10, 26, 10,
    ...chunk("IHDR", ihdr), ...chunk("sRGB", Uint8Array.of(0)),
  );
}

function filterBand(pixels: Rgba, stride: number, previous: Uint8Array): Uint8Array<ArrayBuffer> {
  const rows = pixels.length / stride;
  const filtered = new Uint8Array((stride + 1) * rows);
  for (let y = 0; y < rows; y++) {
    const row = y * stride, out = y * (stride + 1);
    filtered[out] = 4;
    for (let i = 0; i < stride; i++) {
      const a = i >= 4 ? pixels[row + i - 4] : 0;
      const b = y ? pixels[row - stride + i] : previous[i];
      const c = i < 4 ? 0 : y ? pixels[row - stride + i - 4] : previous[i - 4];
      filtered[out + 1 + i] = (pixels[row + i] - paeth(a, b, c)) & 255;
    }
  }
  previous.set(pixels.subarray(pixels.length - stride));
  return filtered;
}

function compressBands(
  bands: Iterable<Rgba> | AsyncIterable<Rgba>, width: number, height: number,
): ReadableStream<Uint8Array<ArrayBuffer>> {
  const stride = width * 4;
  const previous = new Uint8Array(stride);
  const iterator = (async function* () {
    yield* bands;
  })();
  const compressor = new CompressionStream("deflate");
  const reader = compressor.readable.getReader();
  const writer = compressor.writable.getWriter();
  let rows = 0;
  let ended = false;
  let stopped = false;
  let pendingWrite: Promise<void> | undefined;
  let writeFailure: { reason: unknown } | undefined;
  let readingBand = false;

  async function writeBand(): Promise<void> {
    readingBand = true;
    const next = await iterator.next().finally(() => { readingBand = false; });
    if (stopped) return;
    if (next.done) {
      if (rows !== height) throw Error("INVALID_ARGUMENT: PNG pixel count");
      ended = true;
      await writer.close();
      return;
    }
    if (!(next.value instanceof Uint8Array || next.value instanceof Uint8ClampedArray)) {
      throw Error("INVALID_ARGUMENT: PNG band must contain RGBA bytes");
    }
    const count = next.value.length / stride;
    if (!Number.isInteger(count) || count < 1 || count > BAND_ROWS || rows + count > height) {
      throw Error("INVALID_ARGUMENT: PNG band size");
    }
    rows += count;
    await writer.write(filterBand(next.value, stride, previous));
  }

  async function stop(reason: unknown): Promise<void> {
    if (stopped) return;
    stopped = true;
    // An async generator queues return behind a pending next. Close the compressor
    // immediately and let the iterator unwind when that upstream operation settles.
    const returned = iterator.return(undefined).catch(() => {});
    const cleanup = [reader.cancel(reason), writer.abort(reason)];
    if (!readingBand) cleanup.push(returned.then(() => {}));
    await Promise.allSettled(cleanup);
  }

  // Feed a band only while compressed data is requested. The compressor may
  // require several bands before emitting bytes, or emit several chunks per write.
  return new ReadableStream<Uint8Array<ArrayBuffer>>({
    async pull(controller) {
      const output = reader.read();
      try {
        while (true) {
          let result;
          if (!pendingWrite && !ended && !stopped) {
            // A write may already have produced multiple queued chunks. Drain
            // those before asking the caller to generate another band.
            result = await Promise.race([output, Promise.resolve(undefined)]);
            if (result === undefined && !stopped) {
              pendingWrite = writeBand().catch((reason: unknown) => {
                writeFailure = { reason };
              });
            }
          }
          result ??= await (pendingWrite ? Promise.race([output, pendingWrite]) : output);
          if (stopped) return;
          if (writeFailure) throw writeFailure.reason;
          if (result === undefined) {
            pendingWrite = undefined;
          } else if (result.done) {
            controller.close();
            return;
          } else {
            controller.enqueue(result.value);
            return;
          }
        }
      } catch (error) {
        await stop(error);
        throw error;
      }
    },
    cancel: stop,
  }, { highWaterMark: 0 });
}

/** Encodes consecutive bands of at most 256 straight-alpha RGBA rows into PNG chunks. */
export function encodePngStream(
  bands: Iterable<Rgba> | AsyncIterable<Rgba>, width: number, height: number,
): ReadableStream<Uint8Array<ArrayBuffer>> {
  checkSize(width, height, 65536);
  const reader = compressBands(bands, width, height).getReader();
  let headerPending = true;
  let cancelled = false;
  let bytes = new Uint8Array(0);
  let offset = 0;
  return new ReadableStream<Uint8Array<ArrayBuffer>>({
    async pull(controller) {
      if (headerPending) {
        headerPending = false;
        controller.enqueue(pngHeader(width, height));
        return;
      }
      while (offset === bytes.length) {
        const next = await reader.read();
        if (cancelled) return;
        if (next.done) {
          controller.enqueue(chunk("IEND", new Uint8Array(0)));
          controller.close();
          return;
        }
        bytes = next.value;
        offset = 0;
      }
      const end = Math.min(bytes.length, offset + MAX_IDAT_BYTES);
      controller.enqueue(chunk("IDAT", bytes.subarray(offset, end)));
      offset = end;
    },
    cancel(reason) {
      cancelled = true;
      bytes = new Uint8Array(0);
      return reader.cancel(reason);
    },
  }, { highWaterMark: 0 });
}

/** `pixels` holds `width * height` straight-alpha RGBA pixels, row by row. */
export async function encodePng(pixels: Rgba, width: number, height: number): Promise<Blob> {
  checkSize(width, height, 0x7fff_ffff);
  if (pixels.length !== width * height * 4) throw Error("INVALID_ARGUMENT: PNG pixel count");
  function* bands() {
    const bandBytes = width * BAND_ROWS * 4;
    for (let offset = 0; offset < pixels.length; offset += bandBytes) {
      yield pixels.subarray(offset, offset + bandBytes);
    }
  }
  const compressed = new Uint8Array(await new Response(compressBands(bands(), width, height)).arrayBuffer());
  return new Blob(
    [pngHeader(width, height), chunk("IDAT", compressed), chunk("IEND", new Uint8Array(0))],
    { type: "image/png" },
  );
}
