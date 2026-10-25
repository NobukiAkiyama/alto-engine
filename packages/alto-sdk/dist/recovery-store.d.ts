/** Every namespace with a journal on this origin, its size in bytes and last write time (ms). */
export declare function listRecoveryNamespaces(): Promise<{
    namespace: string;
    bytes: number;
    modified: number;
}[]>;
/** Deletes a namespace's journal. Fails with CONFLICT while an engine uses the namespace. */
export declare function deleteRecovery(namespace: string): Promise<void>;
