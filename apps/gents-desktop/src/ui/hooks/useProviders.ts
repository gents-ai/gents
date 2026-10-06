/* What screens read about inference providers, from the provider store.
   Every panel showing the same agent shows the same read. */
import { useCallback, useEffect } from "react";
import { useStore } from "zustand";

import type {
  BackendUsageView,
  InferenceProviderOption,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { useApp } from "../app/AppContext";

const NO_ACCOUNTS: readonly ProviderAccountView[] = [];
const NO_USAGE: readonly BackendUsageView[] = [];
const NO_PROVIDERS: readonly InferenceProviderOption[] = [];

/** An agent's provider accounts: read while shown, and again whenever the
    client changes. `reload` resolves once the read it asked for has landed. */
export function useAccounts(agentDid: string) {
  const { stores, actions } = useApp();
  const { watchProviderAccounts, loadProviderAccounts } = actions;
  const accounts = useStore(
    stores.providers,
    (state) => state.accounts[agentDid] ?? NO_ACCOUNTS,
  );
  useEffect(() => watchProviderAccounts(agentDid), [watchProviderAccounts, agentDid]);
  const reload = useCallback(
    () => loadProviderAccounts(agentDid),
    [loadProviderAccounts, agentDid],
  );
  return { accounts, reload };
}

/** An agent's usage: read when shown and on Refresh (both skip accounts
    read in the last five minutes); nothing polls, and a snapshot change
    does not read again. */
export function useProviderUsage(agentDid: string) {
  const { stores, actions } = useApp();
  const { loadProviderUsage, refreshProviderUsage } = actions;
  const usage = useStore(
    stores.providers,
    (state) => state.usage[agentDid] ?? NO_USAGE,
  );
  useEffect(() => {
    void loadProviderUsage(agentDid);
  }, [loadProviderUsage, agentDid]);
  const refresh = useCallback(
    async (provider: string | null) => {
      await refreshProviderUsage(agentDid, provider);
    },
    [refreshProviderUsage, agentDid],
  );
  return { usage, refresh };
}

/** The provider catalog the bridge publishes, read once for the app; a
    failed read is said, with a way to ask again. */
export function useSetupCatalog() {
  const { stores, actions } = useApp();
  const { loadSetupCatalog, retrySetupCatalog } = actions;
  const catalog = stores.providers.use.catalog();
  const error = stores.providers.use.catalogError();
  useEffect(() => {
    void loadSetupCatalog();
  }, [loadSetupCatalog]);
  return {
    catalog,
    providers: catalog?.providers ?? NO_PROVIDERS,
    error,
    retry: retrySetupCatalog,
  };
}
