/** A typed event emitter: `E` maps each event name to the value it carries. */
export class Emitter {
    listeners = new Map();
    /** Returns a function that removes the listener. */
    on(name, listener) {
        let set = this.listeners.get(name);
        if (!set)
            this.listeners.set(name, (set = new Set()));
        set.add(listener);
        return () => {
            set.delete(listener);
        };
    }
    /** @internal */
    emit(name, value) {
        for (const listener of this.listeners.get(name) ?? [])
            listener(value);
    }
}
