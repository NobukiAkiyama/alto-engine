import type { EngineCore } from "./engine.js";
import { AltoError } from "./errors.js";
import { validBrushImageSize, type BrushConfig, type BrushCurve, type StreamMessage } from "./protocol.js";
import type { Color, NodeId } from "./types.js";

export interface BrushDynamics {
  /** Defaults to pressure with a 5% floor. Pass null for a constant size. */
  size?: BrushCurve | null;
  /** Multiplies dab opacity. Defaults to a constant factor of 1. */
  opacity?: BrushCurve | null;
  /** Multiplies dab flow. Defaults to a constant factor of 1. */
  flow?: BrushCurve | null;
  /** Adds pen tilt direction to the tip angle. The direction is not adjusted for layer rotation. */
  angleFromTilt?: boolean;
}

function wireCurve(curve: BrushCurve): BrushCurve {
  const { input, points } = curve;
  if (!["pressure", "speed", "tilt"].includes(input) ||
      !Array.isArray(points) || points.length < 2 || points.length > 8 ||
      Array.from(points).some((point, i) => !Array.isArray(point) || point.length !== 2 ||
        ![point[0], point[1]].every(value => Number.isFinite(value) && value >= 0 && value <= 1) ||
        // The renderer stores curve coordinates as f32.
        (i > 0 && Math.fround(points[i - 1][0]) >= Math.fround(point[0]))) ||
      points[0][0] !== 0 || points[points.length - 1][0] !== 1) {
    throw new AltoError("INVALID_ARGUMENT", "brush curve");
  }
  return { input, points: points.map(([x, y]) => [Math.fround(x), Math.fround(y)]) };
}

function wireDynamics(dynamics: BrushDynamics): BrushConfig["dynamics"] {
  const { size, opacity, flow, angleFromTilt } = dynamics;
  return {
    size: size === null ? { input: "pressure", points: [[0, 1], [1, 1]] } : size && wireCurve(size),
    opacity: opacity == null ? undefined : wireCurve(opacity),
    flow: flow == null ? undefined : wireCurve(flow),
    angle_from_tilt: angleFromTilt,
  };
}

/** Reusable R8 coverage image. White is full coverage, black is transparent. */
export interface BrushImage {
  width: number;
  height: number;
  pixels: Uint8Array;
}

export interface BrushOptions {
  layer: NodeId;
  /** Paint the layer's pixels or its mask. */
  target?: "content" | "mask";
  /** Size in document pixels (default 24), hardness 0–1 (0.8), spacing as a fraction of size (0.15). */
  tip?: {
    size?: number;
    hardness?: number;
    spacing?: number;
    /** Rotation in degrees in layer coordinates (default 0). */
    angle?: number;
    /** Ratio of short to long radius, clamped to 0.01–1 (default 1). */
    roundness?: number;
    /** Coverage image stamped at each dab. Its long side follows the tip size. */
    image?: BrushImage;
  };
  color?: Color;
  opacity?: number;
  flow?: number;
  dynamics?: BrushDynamics;
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

/** @internal What a tool session needs from the engine. */
export interface ToolHost {
  request: EngineCore["request"];
  stream(message: StreamMessage, transfer: Transferable[]): void;
  screenToDocument(clientX: number, clientY: number): [number, number];
  presentationReady(): boolean;
  storageError(): AltoError | null;
  ended(session: ToolSession, state: unknown): void;
  reportError(error: AltoError): void;
}

/**
 * A brush or eraser stroke in progress. Pass pointer events or samples to `input`;
 * the engine draws them as they arrive. Another edit on the document first ends the
 * stroke. A Worker restart ends it with `WORKER_CRASHED`.
 */
export class ToolSession {
  private samples: number[] = [];
  private done = false;
  private ending = false;
  private aborted: AltoError | null = null;
  private readonly ready: Promise<unknown>;
  /** @internal */
  constructor(
    private readonly host: ToolHost,
    tool: "brush" | "eraser",
    options: BrushOptions,
  ) {
    const angle = options.tip?.angle ?? 0;
    const roundness = options.tip?.roundness ?? 1;
    if (!Number.isFinite(angle)) throw new AltoError("INVALID_ARGUMENT", "brush angle must be finite");
    if (!Number.isFinite(roundness)) throw new AltoError("INVALID_ARGUMENT", "brush roundness must be finite");
    let tipImage;
    const image = options.tip?.image;
    if (image !== undefined) {
      if (!image || !validBrushImageSize(image.width, image.height) ||
          !(image.pixels instanceof Uint8Array) || image.pixels.length !== image.width * image.height) {
        throw new AltoError("INVALID_ARGUMENT", "brush tip");
      }
      // Transfer a snapshot of the view so callers can reuse their image on later strokes.
      tipImage = { width: image.width, height: image.height, pixels: new Uint8Array(image.pixels).buffer };
    }
    this.ready = host.request("beginBrush", {
      layer: options.layer,
      config: {
        size: options.tip?.size ?? 24,
        hardness: options.tip?.hardness ?? 0.8,
        spacing: options.tip?.spacing ?? 0.15,
        opacity: options.opacity ?? 1,
        flow: options.flow ?? 1,
        dynamics: options.dynamics && wireDynamics(options.dynamics),
        angle,
        roundness,
      },
      color: options.color ?? [0, 0, 0, 1],
      erase: tool === "eraser",
      mask: options.target === "mask",
      tipImage,
    }, tipImage ? [tipImage.pixels] : []);
    // A stroke that cannot start ends here, and reports once: the frame loop keeps
    // flushing until the session ends.
    this.ready.catch((e) => {
      this.abort(e instanceof AltoError ? e : new AltoError("INTERNAL", String(e)));
      host.ended(this, null);
      host.reportError(e);
    });
  }
  get active(): boolean {
    return !this.done;
  }
  input(input: PointerEvent | InputSample): void {
    if (this.done) return;
    if ("clientX" in input) {
      if (!this.host.presentationReady()) throw new AltoError("CONFLICT", "the requested view is not ready for pointer input");
      const events = input.getCoalescedEvents?.() || [input];
      for (const e of events.length ? events : [input]) {
        const p = this.host.screenToDocument(e.clientX, e.clientY);
        this.samples.push(p[0], p[1], e.pointerType === "mouse" ? 1 : e.pressure, e.tiltX, e.tiltY, e.timeStamp);
      }
    } else
      this.samples.push(
        input.x,
        input.y,
        input.pressure ?? 1,
        input.tiltX ?? 0,
        input.tiltY ?? 0,
        input.time ?? performance.now(),
      );
  }
  /** @internal Sends buffered samples; the engine calls it every frame. */
  async flush(): Promise<void> {
    if (this.aborted) return;
    try {
      await this.ready;
    } catch {
      return;
    }
    const failure = this.host.storageError();
    if (failure) throw failure;
    if (!this.samples.length) return;
    const a = new Float32Array(this.samples);
    this.samples = [];
    this.host.stream({ t: "input", samples: a }, [a.buffer]);
  }
  /** Commits the stroke as one edit. */
  async end(): Promise<void> {
    if (this.done) {
      if (this.aborted) throw this.aborted;
      return;
    }
    this.done = true;
    this.ending = true;
    try {
      await this.flush();
      if (this.aborted) throw this.aborted;
      const state = await this.host.request("endBrush");
      this.host.ended(this, state);
    } catch (error) {
      this.host.ended(this, null);
      throw error;
    } finally {this.ending=false;}
  }
  /** Discards the stroke, including a pending GPU readback before CPU commit. */
  async cancel(): Promise<void> {
    if (this.done && !this.ending) return;
    this.done = true;
    this.samples = [];
    try {
      await this.ready;
      await this.host.request("cancelBrush");
    } finally {
      this.host.ended(this, null);
    }
  }
  /** @internal The Worker went away under this stroke. */
  abort(error: AltoError): void {
    this.aborted = error;
    this.done = true;
    this.samples = [];
  }
}
