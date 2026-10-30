import assert from "node:assert/strict";
import { test } from "node:test";
import { UNSTABLE_RENDER_V3, renderVersionUpgrade } from "../dist/render-version.js";

test("the legacy opt-in classifier remains available with version 3 enabled", () => {
  assert.equal(UNSTABLE_RENDER_V3, true);
  assert.equal(renderVersionUpgrade(), "stable");
  assert.equal(renderVersionUpgrade({}), "stable");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: false }), "stable");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: true }), "unstable");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: true }, false), "rejected");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: true }, true), "unstable");
});
