/** @deprecated Version 3 is now the default; retained for API compatibility. */
export const UNSTABLE_RENDER_V3 = true;
/**
 * @deprecated Historical opt-in classifier, retained for API compatibility.
 * `Document.upgradeRenderVersion` now upgrades to version 3 regardless of it.
 */
export function renderVersionUpgrade(options, buildEnabled = UNSTABLE_RENDER_V3) {
    if (!options?.unstableRenderV3)
        return "stable";
    return buildEnabled ? "unstable" : "rejected";
}
