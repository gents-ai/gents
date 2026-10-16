import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

export type Registry<T extends { id: string }> = ReturnType<typeof createRegistry<T>>;

export function createRegistry<T extends { id: string }>() {
  // In registration order; a later contribution with an id replaces the
  // earlier one in place.
  const store = createStore<readonly T[]>(() => []);
  return {
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
    useList: (): readonly T[] => useStore(store),
    useItem: (id: string | null | undefined): T | null =>
      useStore(store, (items) => (id && items.find((x) => x.id === id)) || null),
    // Tests only.
    clear: () => store.setState([], true),
  };
}
