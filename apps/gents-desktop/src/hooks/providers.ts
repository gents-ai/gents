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

export function createProviders({ api, store, client }: ProviderParams) {
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
        return null;
      },
    );
  }

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
    loadProviderAccounts,
    watchProviderAccounts(nodeDid: string) {
      watching.set(nodeDid, (watching.get(nodeDid) ?? 0) + 1);
      void loadProviderAccounts(nodeDid);
      return () => {
        const left = (watching.get(nodeDid) ?? 1) - 1;
        if (left > 0) watching.set(nodeDid, left);
        else watching.delete(nodeDid);
      };
    },
    loadProviderUsage(nodeDid: string) {
      return readUsage(nodeDid, false, null)?.catch(() => undefined);
    },
    refreshProviderUsage(nodeDid: string, provider: string | null) {
      return readUsage(nodeDid, true, provider);
    },
    loadSetupCatalog,
    retrySetupCatalog() {
      providers.catalogFailed(store, null);
      return loadSetupCatalog();
    },
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
    canRetryProviderSave: Boolean(api.retrySaveProviderAccount),
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
    removeProviderAccount(nodeDid: string, credentialId: string) {
      return thenReload(nodeDid, async () => {
        await api.removeProviderAccount?.(nodeDid, credentialId);
      });
    },
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
