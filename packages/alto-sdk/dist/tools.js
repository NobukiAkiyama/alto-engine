import { AltoError } from "./errors.js";
/**
 * A brush or eraser stroke in progress. Pass pointer events or samples to `input`;
 * the engine draws them as they arrive. Another edit on the document first ends the
 * stroke. A Worker restart ends it with `WORKER_CRASHED`.
 */
export class ToolSession {
    host;
    samples = [];
    done = false;
    aborted = null;
    ready;
    /** @internal */
    constructor(host, tool, options) {
        this.host = host;
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
        // A stroke that cannot start ends here, and reports once: the frame loop keeps
        // flushing until the session ends.
        this.ready.catch((e) => {
            this.abort(e instanceof AltoError ? e : new AltoError("INTERNAL", String(e)));
            host.ended(this, null);
            host.reportError(e);
        });
    }
    get active() {
        return !this.done;
    }
    input(input) {
        if (this.done)
            return;
        if ("clientX" in input) {
            const events = input.getCoalescedEvents?.() || [input];
            for (const e of events.length ? events : [input]) {
                const p = this.host.screenToDocument(e.clientX, e.clientY);
                this.samples.push(p[0], p[1], e.pointerType === "mouse" ? 1 : e.pressure, e.tiltX, e.tiltY, e.timeStamp);
            }
        }
        else
            this.samples.push(input.x, input.y, input.pressure ?? 1, input.tiltX ?? 0, input.tiltY ?? 0, input.time ?? performance.now());
    }
    /** @internal Sends buffered samples; the engine calls it every frame. */
    async flush() {
        if (this.aborted)
            return;
        try {
            await this.ready;
        }
        catch {
            return;
        }
        const failure = this.host.storageError();
        if (failure)
            throw failure;
        if (!this.samples.length)
            return;
        const a = new Float32Array(this.samples);
        this.samples = [];
        this.host.stream({ t: "input", samples: a }, [a.buffer]);
    }
    /** Commits the stroke as one edit. */
    async end() {
        if (this.done) {
            if (this.aborted)
                throw this.aborted;
            return;
        }
        this.done = true;
        try {
            await this.flush();
            const state = await this.host.request("endBrush");
            this.host.ended(this, state);
        }
        catch (error) {
            this.host.ended(this, null);
            throw error;
        }
    }
    /** Discards the stroke. */
    async cancel() {
        if (this.done)
            return;
        this.done = true;
        this.samples = [];
        try {
            await this.ready;
            await this.host.request("cancelBrush");
        }
        finally {
            this.host.ended(this, null);
        }
    }
    /** @internal The Worker went away under this stroke. */
    abort(error) {
        this.aborted = error;
        this.done = true;
        this.samples = [];
    }
}
