import { encodePngStream } from "./png.js";
import type { Rect } from "./types.js";

export function exportGrid(region: Rect, scale: number) {
  const level = Math.max(0, Math.min(12, Math.floor(Math.log2(1 / scale) + 1e-9)));
  const factor = 2 ** level;
  const x0 = Math.floor(region.x0 / factor), y0 = Math.floor(region.y0 / factor);
  const x1 = Math.ceil(region.x1 / factor), y1 = Math.ceil(region.y1 / factor);
  const width = x1 - x0, height = y1 - y0;
  const ow = Math.max(1, Math.round((region.x1 - region.x0) * scale));
  const oh = Math.max(1, Math.round((region.y1 - region.y0) * scale));
  const exact = region.x0 / factor === x0 && region.y0 / factor === y0 && ow === width && oh === height;
  return { level, factor, x0, y0, x1, y1, width, height, ow, oh, exact };
}

type ReadTiles = (level: number, coords: Int32Array<ArrayBuffer>) => Promise<Uint8Array>;
const TILE_BYTES = 256 * 256 * 4;

export async function readExportPixels(
  region: Rect, level: number, capacity: number, readTiles: ReadTiles,
): Promise<Uint8ClampedArray<ArrayBuffer>> {
  const { x0, x1, y0, y1 } = region;
  const width = x1 - x0;
  const pixels = new Uint8ClampedArray(width * (y1 - y0) * 4);
  const coords: number[] = [];
  const batchSize = 2 * Math.max(1, Math.min(64, capacity));
  const readBatch = async () => {
    const bytes = await readTiles(level, Int32Array.from(coords));
    for (let i = 0; i < coords.length; i += 2) {
      const tx = coords[i] * 256, ty = coords[i + 1] * 256;
      const left = Math.max(x0, tx), right = Math.min(x1, tx + 256);
      const top = Math.max(y0, ty), bottom = Math.min(y1, ty + 256);
      for (let y = top; y < bottom; y++) {
        for (let x = left; x < right; x++) {
          const from = i / 2 * TILE_BYTES + ((y - ty) * 256 + x - tx) * 4;
          const to = ((y - y0) * width + x - x0) * 4;
          const alpha = bytes[from + 3];
          pixels[to + 3] = alpha;
          if (alpha) {
            for (let c = 0; c < 3; c++) pixels[to + c] = Math.round(bytes[from + c] * 255 / alpha);
          }
        }
      }
    }
    coords.length = 0;
  };
  for (let ty = Math.floor(y0 / 256); ty * 256 < y1; ty++) {
    for (let tx = Math.floor(x0 / 256); tx * 256 < x1; tx++) {
      coords.push(tx, ty);
      if (coords.length === batchSize) await readBatch();
    }
  }
  if (coords.length) await readBatch();
  return pixels;
}

type Band = Uint8ClampedArray<ArrayBuffer>;
type Supply = (band: Band | null) => void;

function bandRequest() {
  let resolve!: (supply: Supply) => void;
  const promise = new Promise<Supply>((accept) => { resolve = accept; });
  return { promise, resolve };
}

/** The compressor can request bands asynchronously. Only a Worker pull may
 * fulfill a request, so no GPU read can outlive the serialized Worker command. */
export class PngExport {
  private needBand = bandRequest();
  private waiting?: Supply;
  private reader: ReadableStreamDefaultReader<Uint8Array<ArrayBuffer>>;
  private output?: Promise<ReadableStreamReadResult<Uint8Array<ArrayBuffer>>>;
  private y: number;
  private closed = false;

  constructor(private readonly grid: ReturnType<typeof exportGrid>) {
    this.y = grid.y0;
    this.reader = encodePngStream(this.bands(), grid.width, grid.height).getReader();
  }

  private async *bands(): AsyncGenerator<Band> {
    while (this.y < this.grid.y1 && !this.closed) {
      const band = await new Promise<Band | null>((resolve) => {
        this.waiting = resolve;
        this.needBand.resolve(resolve);
      });
      this.waiting = undefined;
      if (this.closed || !band) return;
      yield band;
    }
  }

  private checkOpen() {
    if (this.closed) throw Error("CONFLICT: document-changed: the PNG export was closed");
  }

  async pull(read: (region: Rect, level: number) => Promise<Band>): Promise<Uint8Array<ArrayBuffer> | null> {
    this.checkOpen();
    this.output ??= this.reader.read();
    for (let supplied = false;; supplied = true) {
      const next = await Promise.race([this.output, this.needBand.promise]);
      this.checkOpen();
      if (typeof next !== "function") {
        this.output = undefined;
        return next.done ? null : next.value;
      }
      // Give queued edits/frames a chance to run even on a highly compressible PNG.
      if (supplied) return new Uint8Array(0);
      const { x0, x1, y1, level } = this.grid;
      const bottom = Math.min(y1, (Math.floor(this.y / 256) + 1) * 256);
      const band = await read({ x0, x1, y0: this.y, y1: bottom }, level);
      this.checkOpen();
      this.y = bottom;
      this.needBand = bandRequest();
      next(band);
    }
  }

  async close() {
    this.closed = true;
    this.waiting?.(null);
    await this.reader.cancel().catch(() => {});
  }
}
