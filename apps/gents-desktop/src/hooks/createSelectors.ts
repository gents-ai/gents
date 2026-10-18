import { useStore } from "zustand";
import type { StoreApi } from "zustand/vanilla";

export type WithSelectors<S> = S extends { getState: () => infer T }
  ? S & { use: { [K in keyof T]: () => T[K] } }
  : never;

export function createSelectors<T extends object>(
  store: StoreApi<T>,
): WithSelectors<StoreApi<T>> {
  const use = {} as { [K in keyof T]: () => T[K] };
  for (const key of Object.keys(store.getState()) as (keyof T)[]) {
    use[key] = () => useStore(store, (state) => state[key]);
  }
  return Object.assign(store, { use }) as WithSelectors<StoreApi<T>>;
}
