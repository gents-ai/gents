import { useStore } from "zustand";
import type { StoreApi } from "zustand/vanilla";

/** A store with a hook per top-level field: `store.use.field()`. */
export type WithSelectors<S> = S extends { getState: () => infer T }
  ? S & { use: { [K in keyof T]: () => T[K] } }
  : never;

/**
 * Adds `store.use.field()` for each field the store starts with: a hook
 * that re-renders its caller when that one field changes by identity.
 * A read that combines or derives fields keeps its own selector.
 * https://zustand.docs.pmnd.rs/learn/guides/auto-generating-selectors
 */
export function createSelectors<T extends object>(
  store: StoreApi<T>,
): WithSelectors<StoreApi<T>> {
  const use = {} as { [K in keyof T]: () => T[K] };
  for (const key of Object.keys(store.getState()) as (keyof T)[]) {
    use[key] = () => useStore(store, (state) => state[key]);
  }
  return Object.assign(store, { use }) as WithSelectors<StoreApi<T>>;
}
