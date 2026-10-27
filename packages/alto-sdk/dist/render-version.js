/**
 * Render version 3 is unstable until integer compositing lands.
 * This constant is false in the source that ships, including release artifacts.
 * Tests pass `buildEnabled: true` to {@link renderVersionUpgrade}; they do not
 * flip this constant.
 */
export const UNSTABLE_RENDER_V3 = false;
/**
 * `stable` keeps the version 2 upgrade. `unstable` is the opted-in version 3
 * path. `rejected` is a request for version 3 in a build that does not have it.
 */
export function renderVersionUpgrade(options, buildEnabled = UNSTABLE_RENDER_V3) {
    if (!options?.unstableRenderV3)
        return "stable";
    return buildEnabled ? "unstable" : "rejected";
}
