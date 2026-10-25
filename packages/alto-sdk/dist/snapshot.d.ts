import type { AltoWarning, DocNode, DocumentAsset, DocumentMeta, NodeId, TileRef, VersionStatus } from "./types.js";
/** The document as it is now. Frozen; nodes that did not change keep their identity. */
export interface DocumentSnapshot {
    readonly revision: number;
    readonly meta: DocumentMeta;
    readonly root: NodeId;
    readonly nodes: ReadonlyMap<NodeId, DocNode>;
    readonly selection: TileRef | null;
    readonly assets: readonly DocumentAsset[];
    readonly history: {
        readonly undo: number;
        readonly redo: number;
    };
    readonly versions: VersionStatus;
    /** True when the document changed since it was last saved. */
    readonly dirty: boolean;
    /** Identifies the content and the version head; changes with every edit. */
    readonly stateHash: string;
    readonly warnings: readonly AltoWarning[];
}
/** What one update changed, so a host redraws only that. */
export interface DocumentChange {
    readonly revision: number;
    readonly nodes: {
        readonly added: NodeId[];
        readonly updated: NodeId[];
        readonly removed: NodeId[];
    };
    readonly meta: boolean;
    readonly selection: boolean;
    readonly assets: boolean;
    readonly history: boolean;
    readonly versions: boolean;
    readonly dirty: boolean;
}
