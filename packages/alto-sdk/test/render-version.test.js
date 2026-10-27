import assert from "node:assert/strict";
import { test } from "node:test";
import { UNSTABLE_RENDER_V3, renderVersionUpgrade } from "../dist/render-version.js";

test("the shipped SDK cannot upgrade to render version 3 or save format 4", () => {
  assert.equal(UNSTABLE_RENDER_V3, false);
  assert.equal(renderVersionUpgrade(), "stable");
  assert.equal(renderVersionUpgrade({}), "stable");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: false }), "stable");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: true }), "rejected");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: true }, false), "rejected");
  assert.equal(renderVersionUpgrade({ unstableRenderV3: true }, true), "unstable");
});
