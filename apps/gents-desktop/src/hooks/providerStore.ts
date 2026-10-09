import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  BackendUsageView,
  InferenceSetupCatalog,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { createSelectors, type WithSelectors } from "./createSelectors";

/** What the bridge says about inference providers, held once for every
    panel that shows it: each agent's accounts and usage as last read, and
    the setup catalog. A read replaces its agent's list whole. */
export type ProviderState = {
  accounts: Readonly<Record<string, readonly ProviderAccountView[]>>;
  usage: Readonly<Record<string, readonly BackendUsageView[]>>;
  catalog: InferenceSetupCatalog | null;
  /** why the catalog could not be read, as the bridge said it; null once a
      read is asked again. Screens word it. */
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

/** The provider store's changes. */
export const providers = {
  accountsRead(
    store: ProviderStore,
    agentDid: string,
    views: readonly ProviderAccountView[],
  ) {
    store.setState((state) => ({ accounts: { ...state.accounts, [agentDid]: views } }));
  },
  usageRead(
    store: ProviderStore,
    agentDid: string,
    views: readonly BackendUsageView[],
  ) {
    store.setState((state) => ({ usage: { ...state.usage, [agentDid]: views } }));
  },
  /** Also clears a failure an earlier read left showing. */
  catalogRead(store: ProviderStore, catalog: InferenceSetupCatalog) {
    store.setState({ catalog, catalogFailure: null });
  },
  /** null clears the failure as a read is asked again, so Retry does not
      keep showing the error it is answering. */
  catalogFailed(store: ProviderStore, failure: { cause: unknown } | null) {
    store.setState({ catalogFailure: failure });
  },
};
