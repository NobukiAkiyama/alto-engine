/** Where the canvas looks: `center` in document pixels, `zoom` in CSS pixels per document pixel. */
export interface Viewport {
    center: [number, number];
    zoom: number;
    /** Radians, clockwise on screen. */
    rotation: number;
}
/** Moves and scales the view. All screen positions are client (CSS) coordinates. */
export declare class ViewportControl {
    private readonly host;
    private view;
    private version;
    get(): Viewport;
    /** The renderer draws device zooms from 0.01 to 128. */
    private clamp;
    /** Zoom is limited to what the renderer draws: 0.01–128 device pixels per document pixel. */
    set(next: Partial<Viewport>): void;
    /** Scales by `factor` keeping the document point under (clientX, clientY) in place. */
    zoomAt(clientX: number, clientY: number, factor: number): void;
    /** Moves the content by a screen distance in CSS pixels, as a drag would. */
    panBy(dxCss: number, dyCss: number): void;
    /** Shows the whole document, unrotated, with `padding` CSS pixels around it (default 32). */
    fit(options?: {
        padding?: number;
    }): void;
    screenToDocument(clientX: number, clientY: number): [number, number];
    documentToScreen(x: number, y: number): [number, number];
}
