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
 * may be one of them (a sign-in from the CLI or another device): each agent
 * some panel shows is read again when the client's snapshot changes, once,
 * however many panels show it. An account command reads its agent's accounts
 * again once it settles, whether or not it succeeded.
 */
export function createProviders({ api, store, client }: ProviderParams) {
  /* per agent, only the newest read is shown, so an older answer, or a
     failure, landing after it changes nothing */
  const accountReads = newestWinsBy<string>();
  const watching = new Map<string, number>();
  const usageReads = newestWinsBy<string>();

  function loadProviderAccounts(
    agentDid: string,
  ): Promise<ProviderAccountView[] | null> {
    const current = accountReads.begin(agentDid);
    return (api.listProviderAccounts?.(agentDid) ?? Promise.resolve([])).then(
      (views) => {
        if (current()) providers.accountsRead(store, agentDid, views);
        return views;
      },
      () => {
        if (current()) providers.accountsRead(store, agentDid, []);
        return null;
      },
    );
  }

  /* An account command settles as the bridge answers it; its agent's
     accounts are read again after, without holding it up. */
  function thenReload<T>(agentDid: string, command: () => Promise<T>) {
    return command().finally(() => void loadProviderAccounts(agentDid));
  }

  function login(provider: OauthProvider, agentDid: string, label: string | null) {
    switch (provider) {
      case "openai":
        return api.codexLogin(agentDid, null, label);
      case "anthropic":
        return api.claudeLogin(agentDid, null, label);
      case "grok":
        return api.grokLogin(agentDid, null, label);
    }
  }

  client.subscribe((state, prev) => {
    if (state.snapshot === prev.snapshot) return;
    for (const agentDid of watching.keys()) void loadProviderAccounts(agentDid);
  });

  function readUsage(agentDid: string, force: boolean, provider: string | null) {
    const current = usageReads.begin(agentDid);
    return api.readProviderUsage?.(agentDid, force, provider).then((views) => {
      if (current()) providers.usageRead(store, agentDid, views);
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
    /** Reads an agent's provider accounts again; resolves once this read
        has landed, to what it read, or null when it failed. Only the newest
        read for the agent is shown, and a failed one shows none. */
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
    /** Signs an agent in to a provider through the browser, under `label`
        when given. `onUrl` hears the sign-in URL while the login runs. */
    signInToProvider(
      agentDid: string,
      provider: OauthProvider,
      {
        label = null,
        onUrl,
      }: { label?: string | null; onUrl?: (url: string) => void } = {},
    ) {
      return thenReload(agentDid, async () => {
        const unwatch = onUrl
          ? await (api.watchProviderLoginUrl?.(provider, onUrl) ?? (() => {}))
          : () => {};
        try {
          return await login(provider, agentDid, label);
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
    retrySaveProviderAccount(agentDid: string, credentialKind: string) {
      return thenReload(agentDid, async () => {
        if (!api.retrySaveProviderAccount)
          throw new Error("Saving a sign-in again is not available");
        return api.retrySaveProviderAccount(agentDid, credentialKind);
      });
    },
    renameProviderAccount(agentDid: string, credentialId: string, label: string) {
      return thenReload(agentDid, async () => {
        await api.renameProviderAccount?.(agentDid, credentialId, label);
      });
    },
    disconnectProviderAccount(agentDid: string, credentialId: string) {
      return thenReload(agentDid, async () => {
        await api.disconnectProviderAccount?.(agentDid, credentialId);
      });
    },
    /** Removes the account and the backends its sign-in created that no
        profile uses. */
    removeProviderAccount(agentDid: string, credentialId: string) {
      return thenReload(agentDid, async () => {
        await api.removeProviderAccount?.(agentDid, credentialId);
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
