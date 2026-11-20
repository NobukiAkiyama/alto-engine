/**
 * The JS half of TIFF export: straight-alpha RGBA rows are split into 256-row
 * strips (the display tile height), optionally Deflated one strip at a time, and
 * packed behind the header Rust builds. Pixels never enter WASM; only the strip
 * table does.
 */
export type TiffCompression = "none" | "deflate";
/** Rows per strip, equal to the height of a display tile. */
export const TIFF_STRIP_ROWS = 256;

export interface TiffStrip {
  rows: number;
  bytes: number;
  /** Zlib bytes (Blob) when Deflated; straight RGBA rows otherwise. */
  data: Uint8Array<ArrayBuffer> | Blob;
}

function deflate(bytes: Uint8Array<ArrayBuffer>): Promise<Blob> {
  // "deflate" is the zlib wrapper, which is TIFF's Adobe Deflate (Compression 8).
  const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream("deflate"));
  // `Response.blob` hands the compressed bytes straight to the Blob assembled at
  // the end; reading an ArrayBuffer first would hold the output twice.
  return new Response(stream).blob();
}

/**
 * `pixels` holds `width * height` straight-alpha RGBA pixels, row by row. Every
 * strip keeps its rows in order; compressed strips hold zlib bytes.
 */
export async function tiffStrips(
  pixels: Uint8Array<ArrayBuffer> | Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
  compression: TiffCompression,
): Promise<TiffStrip[]> {
  if (!(Number.isInteger(width) && Number.isInteger(height) && width > 0 && height > 0))
    throw Error("INVALID_ARGUMENT: TIFF size");
  if (pixels.length !== width * height * 4) throw Error("INVALID_ARGUMENT: TIFF pixel count");
  if (compression !== "none" && compression !== "deflate")
    throw Error("INVALID_ARGUMENT: TIFF compression");
  const stride = width * 4;
  const strips: TiffStrip[] = [];
  for (let y = 0; y < height; y += TIFF_STRIP_ROWS) {
    const rows = Math.min(TIFF_STRIP_ROWS, height - y);
    const raw = new Uint8Array(pixels.buffer, pixels.byteOffset + y * stride, rows * stride);
    if (compression === "deflate") {
      const data = await deflate(raw);
      strips.push({ rows, bytes: data.size, data });
    } else {
      strips.push({ rows, bytes: raw.length, data: raw });
    }
  }
  return strips;
}

/** The strip table `Runtime.tiffHeader` reads: `[{"rows":256,"bytes":...}, ...]`. */
export function tiffStripsJson(strips: TiffStrip[]): string {
  return JSON.stringify(strips.map(({ rows, bytes }) => ({ rows, bytes })));
}

/** The header and IFD from Rust, followed by each strip's bytes in order. */
export function tiffBlob(header: Uint8Array, strips: TiffStrip[]): Blob {
  return new Blob([new Uint8Array(header), ...strips.map((strip) => strip.data)], { type: "image/tiff" });
}
