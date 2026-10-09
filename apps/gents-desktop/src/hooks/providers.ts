import type {
  DesktopApiAdapter,
  InferenceBackendRecommendationRequest,
  InferenceDiscoveryRequest,
  InferenceRecommendationRequest,
  OauthProvider,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { newestWinsBy, singleFlight } from "../lib/reads";
import type { ClientStore } from "./clientStore";
import { providers, type ProviderStore } from "./providerStore";

type ProviderParams = {
  api: DesktopApiAdapter;
  store: ProviderStore;
  client: ClientStore;
};

/**
 * Inference providers for the app, made once: the reads behind the provider
 * store, and the commands that sign in to a provider and manage its accounts.
 * Accounts are not part of a node's view, so any change the client reports
 * may be one of them (a sign-in from the CLI or another device): each node
 * some panel shows is read again when the client's snapshot changes, once,
 * however many panels show it. An account command reads its node's accounts
 * again once it settles, whether or not it succeeded.
 */
export function createProviders({ api, store, client }: ProviderParams) {
  /* per node, only the newest read is shown, so an older answer, or a
     failure, landing after it changes nothing */
  const accountReads = newestWinsBy<string>();
  const watching = new Map<string, number>();
  const usageReads = newestWinsBy<string>();

  function loadProviderAccounts(
    nodeDid: string,
  ): Promise<ProviderAccountView[] | null> {
    const current = accountReads.begin(nodeDid);
    return (api.listProviderAccounts?.(nodeDid) ?? Promise.resolve([])).then(
      (views) => {
        if (current()) providers.accountsRead(store, nodeDid, views);
        return views;
      },
      () => {
        if (current()) providers.accountsRead(store, nodeDid, []);
        return null;
      },
    );
  }

  /* An account command settles as the bridge answers it; its node's
     accounts are read again after, without holding it up. */
  function thenReload<T>(nodeDid: string, command: () => Promise<T>) {
    return command().finally(() => void loadProviderAccounts(nodeDid));
  }

  function login(provider: OauthProvider, nodeDid: string, label: string | null) {
    switch (provider) {
      case "openai":
        return api.codexLogin(nodeDid, null, label);
      case "anthropic":
        return api.claudeLogin(nodeDid, null, label);
      case "grok":
        return api.grokLogin(nodeDid, null, label);
    }
  }

  client.subscribe((state, prev) => {
    if (state.snapshot === prev.snapshot) return;
    for (const nodeDid of watching.keys()) void loadProviderAccounts(nodeDid);
  });

  function readUsage(nodeDid: string, force: boolean, provider: string | null) {
    const current = usageReads.begin(nodeDid);
    return api.readProviderUsage?.(nodeDid, force, provider).then((views) => {
      if (current()) providers.usageRead(store, nodeDid, views);
    });
  }

  const readCatalog = singleFlight(() =>
    Promise.resolve()
      .then(() => api.getInferenceSetupCatalog?.())
      .then(
        (catalog) => {
          if (catalog) providers.catalogRead(store, catalog);
        },
        (cause: unknown) => providers.catalogFailed(store, { cause }),
      ),
  );

  function loadSetupCatalog(): Promise<void> {
    if (store.getState().catalog) return Promise.resolve();
    return readCatalog();
  }

  return {
    /** Reads an node's provider accounts again; resolves once this read
        has landed, to what it read, or null when it failed. Only the newest
        read for the node is shown, and a failed one shows none. */
    loadProviderAccounts,
    /** Shows an node's accounts: they are read now, and again whenever the
        client's snapshot changes, until every watcher has let go. */
    watchProviderAccounts(nodeDid: string) {
      watching.set(nodeDid, (watching.get(nodeDid) ?? 0) + 1);
      void loadProviderAccounts(nodeDid);
      return () => {
        const left = (watching.get(nodeDid) ?? 1) - 1;
        if (left > 0) watching.set(nodeDid, left);
        else watching.delete(nodeDid);
      };
    },
    /** Reads an node's usage, skipping accounts read in the last five
        minutes; a failure keeps the last usage shown. */
    loadProviderUsage(nodeDid: string) {
      return readUsage(nodeDid, false, null)?.catch(() => undefined);
    },
    /** Reads an node's usage now, for one provider or all; rejects when the
        read fails. Only the newest read for the node is shown. */
    refreshProviderUsage(nodeDid: string, provider: string | null) {
      return readUsage(nodeDid, true, provider);
    },
    /** Reads the setup catalog unless it is already held. */
    loadSetupCatalog,
    /** Asks for the catalog again after a failed read. */
    retrySetupCatalog() {
      providers.catalogFailed(store, null);
      return loadSetupCatalog();
    },
    /** Signs a node in to a provider through the browser, under `label`
        when given. `onUrl` hears the sign-in URL while the login runs. */
    signInToProvider(
      nodeDid: string,
      provider: OauthProvider,
      {
        label = null,
        onUrl,
      }: { label?: string | null; onUrl?: (url: string) => void } = {},
    ) {
      return thenReload(nodeDid, async () => {
        const unwatch = onUrl
          ? await (api.watchProviderLoginUrl?.(provider, onUrl) ?? (() => {}))
          : () => {};
        try {
          return await login(provider, nodeDid, label);
        } finally {
          unwatch();
        }
      });
    },
    /** Stops a provider's browser login that is waiting for the person. */
    cancelProviderSignIn(provider: OauthProvider) {
      switch (provider) {
        case "openai":
          return api.cancelCodexLogin();
        case "anthropic":
          return api.cancelClaudeLogin();
        case "grok":
          return api.cancelGrokLogin();
      }
    },
    /** Whether a sign-in whose credential failed to save can be saved again
        without repeating the browser login. */
    canRetryProviderSave: Boolean(api.retrySaveProviderAccount),
    /** Saves the sign-in the bridge holds after a failed credential save.
        `credentialKind` is the account's provider, as the account names it. */
    retrySaveProviderAccount(nodeDid: string, credentialKind: string) {
      return thenReload(nodeDid, async () => {
        if (!api.retrySaveProviderAccount)
          throw new Error("Saving a sign-in again is not available");
        return api.retrySaveProviderAccount(nodeDid, credentialKind);
      });
    },
    renameProviderAccount(nodeDid: string, credentialId: string, label: string) {
      return thenReload(nodeDid, async () => {
        await api.renameProviderAccount?.(nodeDid, credentialId, label);
      });
    },
    disconnectProviderAccount(nodeDid: string, credentialId: string) {
      return thenReload(nodeDid, async () => {
        await api.disconnectProviderAccount?.(nodeDid, credentialId);
      });
    },
    /** Removes the account and the backends its sign-in created that no
        profile uses. */
    removeProviderAccount(nodeDid: string, credentialId: string) {
      return thenReload(nodeDid, async () => {
        await api.removeProviderAccount?.(nodeDid, credentialId);
      });
    },
    /* Asked by the screen that shows the answer, which keeps it: no store. */
    discoverInferenceModels: (request: InferenceDiscoveryRequest) =>
      api.discoverInferenceModels(request),
    probeInferenceEndpoint: (endpoint: string) => api.probeInferenceEndpoint(endpoint),
    getInferenceModelRecommendation: (request: InferenceRecommendationRequest) =>
      api.getInferenceModelRecommendation(request),
    getInferenceBackendRecommendation: (
      request: InferenceBackendRecommendationRequest,
    ) => api.getInferenceBackendRecommendation(request),
  };
}
