/** A typed event emitter: `E` maps each event name to the value it carries. */
export class Emitter<E> {
  private listeners = new Map<keyof E, Set<(value: never) => void>>();
  /** Returns a function that removes the listener. */
  on<K extends keyof E>(name: K, listener: (value: E[K]) => void): () => void {
    let set = this.listeners.get(name);
    if (!set) this.listeners.set(name, (set = new Set()));
    set.add(listener as (value: never) => void);
    return () => {
      set.delete(listener as (value: never) => void);
    };
  }
  /** @internal */
  emit<K extends keyof E>(name: K, value: E[K]): void {
    for (const listener of this.listeners.get(name) ?? []) (listener as (value: E[K]) => void)(value);
  }
}
