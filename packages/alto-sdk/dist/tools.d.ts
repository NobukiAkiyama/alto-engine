import type { Color, NodeId } from "./types.js";
export interface BrushOptions {
    layer: NodeId;
    /** Paint the layer's pixels or its mask. */
    target?: "content" | "mask";
    /** Size in document pixels (default 24), hardness 0–1 (0.8), spacing as a fraction of size (0.15). */
    tip?: {
        size?: number;
        hardness?: number;
        spacing?: number;
    };
    color?: Color;
    opacity?: number;
    flow?: number;
    /** Pen pressure scales the size (default true). */
    pressureSize?: boolean;
    /** Pen pressure scales the opacity (default false). */
    pressureOpacity?: boolean;
}
/** One input sample in document coordinates. */
export interface InputSample {
    x: number;
    y: number;
    pressure?: number;
    tiltX?: number;
    tiltY?: number;
    /** `performance.now()` time; defaults to now. */
    time?: number;
}
/**
 * A brush or eraser stroke in progress. Pass pointer events or samples to `input`;
 * the engine draws them as they arrive. Another edit on the document first ends the
 * stroke. A Worker restart ends it with `WORKER_CRASHED`.
 */
export declare class ToolSession {
    private readonly host;
    private samples;
    private done;
    private aborted;
    private readonly ready;
    get active(): boolean;
    input(input: PointerEvent | InputSample): void;
    /** Commits the stroke as one edit. */
    end(): Promise<void>;
    /** Discards the stroke. */
    cancel(): Promise<void>;
}
