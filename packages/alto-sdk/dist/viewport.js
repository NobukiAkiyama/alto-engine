/** Moves and scales the view. All screen positions are client (CSS) coordinates. */
export class ViewportControl {
    host;
    view = { center: [512, 384], zoom: 1, rotation: 0 };
    version = 0;
    /** @internal */
    constructor(host) {
        this.host = host;
    }
    get() {
        return { center: [...this.view.center], zoom: this.view.zoom, rotation: this.view.rotation };
    }
    set(next) {
        if (next.zoom !== undefined && !(next.zoom > 0 && Number.isFinite(next.zoom)))
            throw new RangeError("zoom must be a positive number");
        this.view = {
            center: next.center ? [next.center[0], next.center[1]] : this.view.center,
            zoom: next.zoom ?? this.view.zoom,
            rotation: next.rotation ?? this.view.rotation,
        };
        this.publish();
    }
    /** Scales by `factor` keeping the document point under (clientX, clientY) in place. */
    zoomAt(clientX, clientY, factor) {
        const before = this.screenToDocument(clientX, clientY);
        this.view.zoom *= factor;
        const after = this.screenToDocument(clientX, clientY);
        this.view.center = [
            this.view.center[0] + before[0] - after[0],
            this.view.center[1] + before[1] - after[1],
        ];
        this.publish();
    }
    /** Moves the content by a screen distance in CSS pixels, as a drag would. */
    panBy(dxCss, dyCss) {
        const c = Math.cos(this.view.rotation), s = Math.sin(this.view.rotation);
        const dx = dxCss / this.view.zoom, dy = dyCss / this.view.zoom;
        this.view.center = [this.view.center[0] - (c * dx + s * dy), this.view.center[1] - (-s * dx + c * dy)];
        this.publish();
    }
    /** Shows the whole document, unrotated, with `padding` CSS pixels around it (default 32). */
    fit(options = {}) {
        const size = this.host.documentSize();
        if (!size)
            return;
        const r = this.host.rect();
        const pad = options.padding ?? 32;
        const w = Math.max(1, r.width - 2 * pad), h = Math.max(1, r.height - 2 * pad);
        const zoom = r.width > 0 && r.height > 0 ? Math.min(w / size[0], h / size[1]) : 1;
        this.view = { center: [size[0] / 2, size[1] / 2], zoom, rotation: 0 };
        this.publish();
    }
    screenToDocument(clientX, clientY) {
        const r = this.host.rect();
        const dx = (clientX - r.left - r.width / 2) / this.view.zoom;
        const dy = (clientY - r.top - r.height / 2) / this.view.zoom;
        const c = Math.cos(this.view.rotation), s = Math.sin(this.view.rotation);
        return [this.view.center[0] + c * dx + s * dy, this.view.center[1] - s * dx + c * dy];
    }
    documentToScreen(x, y) {
        const r = this.host.rect();
        const ux = x - this.view.center[0], uy = y - this.view.center[1];
        const c = Math.cos(this.view.rotation), s = Math.sin(this.view.rotation);
        // Inverse of the rotation in screenToDocument.
        const dx = c * ux - s * uy, dy = s * ux + c * uy;
        return [r.left + r.width / 2 + dx * this.view.zoom, r.top + r.height / 2 + dy * this.view.zoom];
    }
    /** @internal Sends the current view again, after a Worker restart or a pixel ratio change. */
    publish() {
        this.version++;
        this.host.send({ ...this.get(), version: this.version, deviceZoom: this.view.zoom * this.host.pixelRatio() });
        this.host.changed(this.get());
    }
}
