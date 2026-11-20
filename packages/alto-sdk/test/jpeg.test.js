import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_LOSSY_QUALITY, compositeOverBackground, lossyQuality } from "../dist/jpeg.js";

function pixel(r, g, b, a) {
  return new Uint8ClampedArray([r, g, b, a]);
}

test("JPEG composites alpha over white by default", () => {
  const pixels = new Uint8ClampedArray([...pixel(255, 0, 0, 0), ...pixel(255, 0, 0, 128)]);
  compositeOverBackground(pixels);
  assert.deepEqual([...pixels.subarray(0, 4)], [255, 255, 255, 255]);
  // (255 * 128 + 255 * 127) / 255 = 255; (0 * 128 + 255 * 127) / 255 = 127.
  assert.deepEqual([...pixels.subarray(4)], [255, 127, 127, 255]);
});

test("JPEG composites alpha over a chosen background in integer math", () => {
  const pixels = new Uint8ClampedArray([...pixel(255, 0, 0, 128), ...pixel(10, 20, 30, 255)]);
  compositeOverBackground(pixels, [0, 0, 0]);
  // (255 * 128 + 0 * 127) / 255 = 128, rounded.
  assert.deepEqual([...pixels.subarray(0, 4)], [128, 0, 0, 255]);
  assert.deepEqual([...pixels.subarray(4)], [10, 20, 30, 255]);
});

test("JPEG rounds a fractional background before compositing", () => {
  const pixels = pixel(0, 0, 0, 0);
  compositeOverBackground(pixels, [0.5, 0.25, 0.1]);
  assert.deepEqual([...pixels], [128, 64, 26, 255]);
});

test("quality defaults to 0.92 for JPEG and WebP only", () => {
  assert.equal(lossyQuality("image/jpeg"), DEFAULT_LOSSY_QUALITY);
  assert.equal(lossyQuality("image/webp", undefined), DEFAULT_LOSSY_QUALITY);
  assert.equal(lossyQuality("image/jpeg", 0.5), 0.5);
  assert.equal(lossyQuality("image/png", 0.5), undefined);
  assert.equal(lossyQuality("image/tiff"), undefined);
});
