import type { EngineCore } from "./engine.js";
import { AltoError } from "./errors.js";
import type { StreamMessage } from "./protocol.js";
import type { Color, NodeId } from "./types.js";

export interface BrushOptions {
  layer: NodeId;
  /** Paint the layer's pixels or its mask. */
  target?: "content" | "mask";
  /** Size in document pixels (default 24), hardness 0–1 (0.8), spacing as a fraction of size (0.15). */
  tip?: { size?: number; hardness?: number; spacing?: number };
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

/** @internal What a tool session needs from the engine. */
export interface ToolHost {
  request: EngineCore["request"];
  stream(message: StreamMessage, transfer: Transferable[]): void;
  screenToDocument(clientX: number, clientY: number): [number, number];
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
  private aborted: AltoError | null = null;
  private readonly ready: Promise<unknown>;
  /** @internal */
  constructor(
    private readonly host: ToolHost,
    tool: "brush" | "eraser",
    options: BrushOptions,
  ) {
    this.ready = host.request("beginBrush", {
      layer: options.layer,
      config: {
        size: options.tip?.size ?? 24,
        hardness: options.tip?.hardness ?? 0.8,
        spacing: options.tip?.spacing ?? 0.15,
        opacity: options.opacity ?? 1,
        flow: options.flow ?? 1,
        pressure_size: options.pressureSize ?? true,
        pressure_opacity: options.pressureOpacity ?? false,
      },
      color: options.color ?? [0, 0, 0, 1],
      erase: tool === "eraser",
      mask: options.target === "mask",
    });
    this.ready.catch((e) => host.reportError(e));
  }
  get active(): boolean {
    return !this.done;
  }
  input(input: PointerEvent | InputSample): void {
    if (this.done) return;
    if ("clientX" in input) {
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
    await this.ready;
    if (this.aborted) throw this.aborted;
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
    try {
      await this.flush();
      const state = await this.host.request("endBrush");
      this.host.ended(this, state);
    } catch (error) {
      this.host.ended(this, null);
      throw error;
    }
  }
  /** Discards the stroke. */
  async cancel(): Promise<void> {
    if (this.done) return;
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
