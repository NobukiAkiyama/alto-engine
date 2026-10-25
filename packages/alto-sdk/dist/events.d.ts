/** A typed event emitter: `E` maps each event name to the value it carries. */
export declare class Emitter<E> {
    private listeners;
    /** Returns a function that removes the listener. */
    on<K extends keyof E>(name: K, listener: (value: E[K]) => void): () => void;
}
