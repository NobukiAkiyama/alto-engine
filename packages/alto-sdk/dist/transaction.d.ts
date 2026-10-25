import type { Affine, ClipPath, Effect, FontAsset, ImageAsset, NodeId, PropPatch, StyledPath, TextContent, TileRef, VectorMask } from "./types.js";
export interface InsertOptions {
    /** Defaults to the document root. */
    parent?: NodeId;
    /** Position among the parent's children; defaults to the top. */
    index?: number;
    name?: string;
}
/**
 * The operations of one edit. Build them in the `transact` callback, which must be
 * synchronous; the edit is applied as a whole and undoes as one step.
 */
export declare class Transaction {
    private readonly snapshot;
    addRaster(options?: InsertOptions): NodeId;
    addGroup(options?: InsertOptions & {
        isolated?: boolean;
    }): NodeId;
    /** A raster layer showing an imported image. */
    addRasterFromAsset(asset: ImageAsset, options?: InsertOptions): NodeId;
    /** Registers an imported font in the document; text can use it afterwards. Returns its asset id. */
    addFont(font: FontAsset): string;
    addText(content: TextContent, options?: InsertOptions): NodeId;
    setText(id: NodeId, content: TextContent): void;
    addVector(paths?: StyledPath[], options?: InsertOptions): NodeId;
    setVector(id: NodeId, paths: StyledPath[]): void;
    setVectorMask(id: NodeId, mask: VectorMask | null): void;
    setClipPath(id: NodeId, path: ClipPath | null): void;
    /** An adjustment layer applying `adjustment` to the layers below it. */
    addAdjustment(adjustment: Effect, options?: InsertOptions): NodeId;
    setAdjustment(id: NodeId, adjustment: Effect): void;
    /** Replaces a layer's pixel mask (R8 tiles), or removes it with null. */
    setMask(id: NodeId, mask: TileRef | null): void;
    removeNode(id: NodeId): void;
    moveNode(id: NodeId, parent: NodeId, index: number): void;
    setProps(id: NodeId, patch: PropPatch): void;
    /** Sets the transform from a matrix or from position, scale and rotation (radians). */
    setTransform(id: NodeId, t: Affine | {
        x: number;
        y: number;
        scaleX: number;
        scaleY: number;
        rotation: number;
    }): void;
    private insert;
}
