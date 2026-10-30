/** @deprecated Version 3 is now the default; retained for API compatibility. */
export const UNSTABLE_RENDER_V3 = true;

/** @deprecated Version 3 no longer needs an opt-in. */
export interface UnstableRenderV3Options {
  /** Retained for existing callers; the SDK ignores this value. */
  unstableRenderV3?: boolean;
}

/**
 * @deprecated Historical opt-in classifier, retained for API compatibility.
 * `Document.upgradeRenderVersion` now upgrades to version 3 regardless of it.
 */
export function renderVersionUpgrade(
  options?: UnstableRenderV3Options,
  buildEnabled = UNSTABLE_RENDER_V3,
): "stable" | "unstable" | "rejected" {
  if (!options?.unstableRenderV3) return "stable";
  return buildEnabled ? "unstable" : "rejected";
}
