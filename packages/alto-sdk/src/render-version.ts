/**
 * Render version 3 is unstable until integer compositing lands.
 * This constant is false in the source that ships, including release artifacts.
 * Tests pass `buildEnabled: true` to {@link renderVersionUpgrade}; they do not
 * flip this constant.
 */
export const UNSTABLE_RENDER_V3 = false;

/** Asks to move onto render version 3, or to save a document that is already on it. */
export interface UnstableRenderV3Options {
  /**
   * Request render version 3. Ignored unless `buildEnabled` is true.
   * The shipped SDK passes {@link UNSTABLE_RENDER_V3}, which is false.
   */
  unstableRenderV3?: boolean;
}

/**
 * `stable` keeps the version 2 upgrade. `unstable` is the opted-in version 3
 * path. `rejected` is a request for version 3 in a build that does not have it.
 */
export function renderVersionUpgrade(
  options?: UnstableRenderV3Options,
  buildEnabled = UNSTABLE_RENDER_V3,
): "stable" | "unstable" | "rejected" {
  if (!options?.unstableRenderV3) return "stable";
  return buildEnabled ? "unstable" : "rejected";
}
