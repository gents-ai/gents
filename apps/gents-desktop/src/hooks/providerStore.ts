import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  AccountUsageView,
  InferenceSetupCatalog,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { createSelectors, type WithSelectors } from "./createSelectors";

export type ProviderState = {
  accounts: Readonly<Record<string, readonly ProviderAccountView[]>>;
  usage: Readonly<Record<string, readonly AccountUsageView[]>>;
  catalog: InferenceSetupCatalog | null;
  catalogFailure: { cause: unknown } | null;
};

export type ProviderStore = WithSelectors<StoreApi<ProviderState>>;

export function createProviderStore() {
  return createSelectors(
    createStore<ProviderState>(() => ({
      accounts: {},
      usage: {},
      catalog: null,
      catalogFailure: null,
    })),
  );
}

export const providers = {
  accountsRead(
    store: ProviderStore,
    nodeDid: string,
    views: readonly ProviderAccountView[],
  ) {
    store.setState((state) => ({ accounts: { ...state.accounts, [nodeDid]: views } }));
  },
  usageRead(store: ProviderStore, nodeDid: string, views: readonly AccountUsageView[]) {
    store.setState((state) => ({ usage: { ...state.usage, [nodeDid]: views } }));
  },
  catalogRead(store: ProviderStore, catalog: InferenceSetupCatalog) {
    store.setState({ catalog, catalogFailure: null });
  },
  catalogFailed(store: ProviderStore, failure: { cause: unknown } | null) {
    store.setState({ catalogFailure: failure });
  },
};
