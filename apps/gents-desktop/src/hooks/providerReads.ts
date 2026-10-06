import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

import type { ClientStore } from "./clientStore";
import { providers, type ProviderStore } from "./providerStore";

type ProviderReadParams = {
  api: DesktopApiAdapter;
  store: ProviderStore;
  client: ClientStore;
};

/**
 * The bridge reads behind the provider store, made once for the app.
 * Accounts are not part of a node's view, so any change the client reports
 * may be one of them (a sign-in from the CLI or another device): each agent
 * some panel shows is read again when the client's snapshot changes, once,
 * however many panels show it.
 */
export function createProviderReads({ api, store, client }: ProviderReadParams) {
  /* per agent, the newest read asked for: only it is shown, so an older
     answer, or a failure, landing after it changes nothing */
  const accountReads = new Map<string, number>();
  const watching = new Map<string, number>();
  const usageReads = new Map<string, number>();
  let catalogRead: Promise<void> | null = null;

  function loadProviderAccounts(agentDid: string): Promise<void> {
    const read = (accountReads.get(agentDid) ?? 0) + 1;
    accountReads.set(agentDid, read);
    const current = () => accountReads.get(agentDid) === read;
    return (api.listProviderAccounts?.(agentDid) ?? Promise.resolve([])).then(
      (views) => {
        if (current()) providers.accountsRead(store, agentDid, views);
      },
      () => {
        if (current()) providers.accountsRead(store, agentDid, []);
      },
    );
  }

  client.subscribe((state, prev) => {
    if (state.snapshot === prev.snapshot) return;
    for (const agentDid of watching.keys()) void loadProviderAccounts(agentDid);
  });

  function readUsage(agentDid: string, force: boolean, provider: string | null) {
    const read = (usageReads.get(agentDid) ?? 0) + 1;
    usageReads.set(agentDid, read);
    return api.readProviderUsage?.(agentDid, force, provider).then((views) => {
      if (usageReads.get(agentDid) === read)
        providers.usageRead(store, agentDid, views);
    });
  }

  function loadSetupCatalog(): Promise<void> {
    if (store.getState().catalog) return Promise.resolve();
    catalogRead ??= Promise.resolve()
      .then(() => api.getInferenceSetupCatalog?.())
      .then(
        (catalog) => {
          if (catalog) providers.catalogRead(store, catalog);
        },
        (error: unknown) =>
          providers.catalogFailed(
            store,
            error instanceof Error ? error.message : String(error),
          ),
      )
      .finally(() => {
        catalogRead = null;
      });
    return catalogRead;
  }

  return {
    /** Reads an agent's provider accounts again; resolves once this read
        has landed. Only the newest read for the agent is shown, and a failed
        one shows none. */
    loadProviderAccounts,
    /** Shows an agent's accounts: they are read now, and again whenever the
        client's snapshot changes, until every watcher has let go. */
    watchProviderAccounts(agentDid: string) {
      watching.set(agentDid, (watching.get(agentDid) ?? 0) + 1);
      void loadProviderAccounts(agentDid);
      return () => {
        const left = (watching.get(agentDid) ?? 1) - 1;
        if (left > 0) watching.set(agentDid, left);
        else watching.delete(agentDid);
      };
    },
    /** Reads an agent's usage, skipping accounts read in the last five
        minutes; a failure keeps the last usage shown. */
    loadProviderUsage(agentDid: string) {
      return readUsage(agentDid, false, null)?.catch(() => undefined);
    },
    /** Reads an agent's usage now, for one provider or all; rejects when the
        read fails. Only the newest read for the agent is shown. */
    refreshProviderUsage(agentDid: string, provider: string | null) {
      return readUsage(agentDid, true, provider);
    },
    /** Reads the setup catalog unless it is already held. */
    loadSetupCatalog,
    /** Asks for the catalog again after a failed read. */
    retrySetupCatalog() {
      providers.catalogFailed(store, null);
      return loadSetupCatalog();
    },
  };
}
