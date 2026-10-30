/** @deprecated Version 3 is now the default; retained for API compatibility. */
export declare const UNSTABLE_RENDER_V3 = true;
/** @deprecated Version 3 no longer needs an opt-in. */
export interface UnstableRenderV3Options {
    /** Retained for existing callers; the SDK ignores this value. */
    unstableRenderV3?: boolean;
}
/**
 * @deprecated Historical opt-in classifier, retained for API compatibility.
 * `Document.upgradeRenderVersion` now upgrades to version 3 regardless of it.
 */
export declare function renderVersionUpgrade(options?: UnstableRenderV3Options, buildEnabled?: boolean): "stable" | "unstable" | "rejected";
