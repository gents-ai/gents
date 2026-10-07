/* An extension point: the contributions an area of the UI draws, by id. The
   app registers its own the same way anything added later would, and the
   area reads them through a hook, so one registered after the first render
   still shows. */
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

export type Registry<T extends { id: string }> = ReturnType<typeof createRegistry<T>>;

export function createRegistry<T extends { id: string }>() {
  /* in registration order; a later contribution with an id replaces the earlier one in place */
  const store = createStore<readonly T[]>(() => []);
  return {
    /** Adds a contribution and returns what removes it again. */
    register(item: T): () => void {
      store.setState((items) => {
        const at = items.findIndex((x) => x.id === item.id);
        return at === -1
          ? [...items, item]
          : items.map((x, i) => (i === at ? item : x));
      }, true);
      return () => store.setState((items) => items.filter((x) => x !== item), true);
    },
    get: (id: string | null | undefined): T | null =>
      (id && store.getState().find((x) => x.id === id)) || null,
    list: (): readonly T[] => store.getState(),
    /** Every contribution, in registration order; re-renders when one is added or removed. */
    useList: (): readonly T[] => useStore(store),
    /** One contribution by id; re-renders when it is added, replaced or removed. */
    useItem: (id: string | null | undefined): T | null =>
      useStore(store, (items) => (id && items.find((x) => x.id === id)) || null),
    /** tests only */
    clear: () => store.setState([], true),
  };
}
