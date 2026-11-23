/** `convertToBlob` quality when the caller did not choose one. */
export const DEFAULT_LOSSY_QUALITY = 0.92;

export function lossyQuality(type: string, quality?: number): number | undefined {
  return type === "image/jpeg" || type === "image/webp" ? (quality ?? DEFAULT_LOSSY_QUALITY) : undefined;
}

/**
 * Composite straight-alpha RGBA over an opaque background, in place, for JPEG.
 * `background` is straight sRGB in 0..1; white when omitted. Integer math:
 * `c' = round((c * a + bg * (255 - a)) / 255)`, with `bg` rounded to 0..255
 * first, and `a' = 255`. Fully opaque pixels keep their exact bytes.
 */
export function compositeOverBackground(
  pixels: Uint8Array | Uint8ClampedArray,
  background: readonly [number, number, number] = [1, 1, 1],
): void {
  if (pixels.length % 4 !== 0) throw Error("INVALID_ARGUMENT: JPEG pixel count");
  const bg = background.map((c) => Math.round(c * 255)) as [number, number, number];
  for (let i = 0; i < pixels.length; i += 4) {
    const a = pixels[i + 3];
    if (a === 255) continue;
    const clear = 255 - a;
    for (let c = 0; c < 3; c++) pixels[i + c] = Math.round((pixels[i + c] * a + bg[c] * clear) / 255);
    pixels[i + 3] = 255;
  }
}
