import { describe, expect, it, vi } from "vitest";
import type { ProviderAccountView } from "@source-inc/gents-desktop-client";

import { createClientStore } from "../src/hooks/clientStore";
import { createProviders } from "../src/hooks/providers";
import { createProviderStore } from "../src/hooks/providerStore";

const AGENT = "did:key:agent";
const account = (credentialId: string) => ({ credentialId }) as ProviderAccountView;

/* a promise the test settles */
function later<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}

function owner(api: object) {
  const store = createProviderStore();
  const providers = createProviders({
    api: api as never,
    store,
    client: createClientStore(),
  });
  return { providers, store };
}

describe("the providers' owner", () => {
  it("signs in through the provider's login, hears its URL while it runs, and shows the new account", async () => {
    const unwatch = vi.fn();
    let sendUrl: (url: string) => void = () => {};
    const api = {
      claudeLogin: vi.fn(async () => {
        sendUrl("https://example.test/sign-in");
        expect(unwatch).not.toHaveBeenCalled();
        return { credentialId: "c1" };
      }),
      watchProviderLoginUrl: vi.fn(async (_provider, onUrl) => {
        sendUrl = onUrl;
        return unwatch;
      }),
      listProviderAccounts: vi.fn().mockResolvedValue([account("c1")]),
    };
    const { providers, store } = owner(api);
    const onUrl = vi.fn();

    await providers.signInToProvider(AGENT, "anthropic", { label: "Work", onUrl });

    expect(api.watchProviderLoginUrl).toHaveBeenCalledWith("anthropic", onUrl);
    expect(api.claudeLogin).toHaveBeenCalledWith(AGENT, null, "Work");
    expect(onUrl).toHaveBeenCalledWith("https://example.test/sign-in");
    expect(unwatch).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(store.getState().accounts[AGENT]).toHaveLength(1));
  });

  it("says a failed command at once, and reads the accounts again after it", async () => {
    const accounts = later<ProviderAccountView[]>();
    const api = {
      removeProviderAccount: vi.fn().mockRejectedValue(new Error("in use")),
      listProviderAccounts: vi.fn(() => accounts.promise),
    };
    const { providers, store } = owner(api);

    await expect(providers.removeProviderAccount(AGENT, "c1")).rejects.toThrow(
      "in use",
    );
    expect(api.listProviderAccounts).toHaveBeenCalledWith(AGENT);

    accounts.resolve([account("c1")]);
    await vi.waitFor(() => expect(store.getState().accounts[AGENT]).toHaveLength(1));
  });

  it("cancels the login of the provider asked for", async () => {
    const api = {
      cancelCodexLogin: vi.fn().mockResolvedValue(undefined),
      cancelClaudeLogin: vi.fn().mockResolvedValue(undefined),
      cancelGrokLogin: vi.fn().mockResolvedValue(undefined),
    };
    const { providers } = owner(api);

    await providers.cancelProviderSignIn("grok");

    expect(api.cancelGrokLogin).toHaveBeenCalledTimes(1);
    expect(api.cancelCodexLogin).not.toHaveBeenCalled();
    expect(api.cancelClaudeLogin).not.toHaveBeenCalled();
  });

  it("gives setup what it read, and null for a read that failed", async () => {
    const api = {
      listProviderAccounts: vi
        .fn()
        .mockResolvedValueOnce([account("c1")])
        .mockRejectedValueOnce(new Error("runtime down")),
    };
    const { providers } = owner(api);

    await expect(providers.loadProviderAccounts(AGENT)).resolves.toEqual([
      account("c1"),
    ]);
    await expect(providers.loadProviderAccounts(AGENT)).resolves.toBeNull();
  });
});
