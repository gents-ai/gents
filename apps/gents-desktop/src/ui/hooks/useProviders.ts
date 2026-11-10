import { useCallback, useEffect, useState } from "react";
import { useStore } from "zustand";

import type {
  AccountUsageView,
  InferenceProviderOption,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { useApp } from "../app/AppContext";
import { setupErrorMessage } from "../../lib/setupErrors";

const NO_ACCOUNTS: readonly ProviderAccountView[] = [];
const NO_USAGE: readonly AccountUsageView[] = [];
const NO_PROVIDERS: readonly InferenceProviderOption[] = [];

export function useAccounts(nodeDid: string) {
  const { stores, actions } = useApp();
  const { watchProviderAccounts, loadProviderAccounts } = actions;
  const accounts = useStore(
    stores.providers,
    (state) => state.accounts[nodeDid] ?? NO_ACCOUNTS,
  );
  useEffect(() => watchProviderAccounts(nodeDid), [watchProviderAccounts, nodeDid]);
  const reload = useCallback(
    () => loadProviderAccounts(nodeDid),
    [loadProviderAccounts, nodeDid],
  );
  return { accounts, reload };
}

export function useProviderUsage(nodeDid: string, backendId?: string) {
  const { stores, actions } = useApp();
  const { loadProviderUsage, refreshProviderUsage } = actions;
  const read = useStore(stores.providers, (state) => state.usage[nodeDid]);
  const usage = read ?? NO_USAGE;
  /* a failed first read settles too, so nothing waits on it for good */
  const [settledFor, setSettledFor] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    void Promise.resolve(loadProviderUsage(nodeDid)).finally(() => {
      if (live) setSettledFor(nodeDid);
    });
    return () => {
      live = false;
    };
  }, [loadProviderUsage, nodeDid, backendId]);
  const loading = read === undefined && settledFor !== nodeDid;
  const refresh = useCallback(
    async (provider: string | null) => {
      await refreshProviderUsage(nodeDid, provider);
    },
    [refreshProviderUsage, nodeDid],
  );
  return { usage, loading, refresh };
}

export function useSetupCatalog() {
  const { stores, actions } = useApp();
  const { loadSetupCatalog, retrySetupCatalog } = actions;
  const catalog = stores.providers.use.catalog();
  const failure = stores.providers.use.catalogFailure();
  useEffect(() => {
    void loadSetupCatalog();
  }, [loadSetupCatalog]);
  return {
    catalog,
    providers: catalog?.providers ?? NO_PROVIDERS,
    error: failure ? setupErrorMessage(failure.cause) : null,
    retry: retrySetupCatalog,
  };
}
