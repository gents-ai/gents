import { describe, expect, it } from "vitest";

import type {
  InferenceDiscoveryResult,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import {
  initialSetupForm,
  setupFormReducer,
  type SetupEvent,
  type SetupForm,
} from "../src/ui/screens/setup/inferenceSetupForm";

const run = (form: SetupForm, ...events: SetupEvent[]) =>
  events.reduce(setupFormReducer, form);
const discovered = { models: [] } as unknown as InferenceDiscoveryResult;
const connection = {
  authMethod: "api_key" as const,
  endpoint: "https://x",
  apiKey: "k",
};

describe("the inference setup form", () => {
  it("is busy while an operation is out, and starting one clears the error", () => {
    const failed = run(initialSetupForm(), { type: "failed", error: "nope" });
    const started = run(failed, { type: "opStarted", op: "discover" });
    expect(started.op).toBe("discover");
    expect(started.error).toBeNull();
    expect(run(started, { type: "opEnded" }).op).toBeNull();
  });

  it("asks for a sign-in again when the provider already shown is chosen again", () => {
    const once = run(initialSetupForm(), {
      type: "providerPicked",
      provider: "openai",
      current: "openai",
    });
    const twice = run(once, {
      type: "providerPicked",
      provider: "openai",
      current: "openai",
    });
    expect(twice.autoSignIn).toEqual({ provider: "openai", asked: 2 });
    expect(twice.provider).toBeNull();
  });

  it("drops what was discovered when the connection changes, keeping the search", () => {
    const searched = run(
      initialSetupForm(),
      { type: "modelsDiscovered", discovery: discovered },
      { type: "manualModelTyped", name: "m" },
      { type: "searchEdited", search: "gpt" },
      { type: "connectionEdited", provider: "openai", connection },
    );
    expect(searched.model.discovery).toBeNull();
    expect(searched.model.name).toBe("");
    expect(searched.model.search).toBe("gpt");
    expect(searched.connections.openai).toEqual(connection);
  });

  it("seeds connected accounts from a read only where asked, and forgets them for another agent", () => {
    const accounts = [
      {
        credentialId: "c-1",
        enabled: true,
        pendingSave: false,
        provider: "ChatGptCodex",
      },
    ] as unknown as ProviderAccountView[];
    const onboarding = run(initialSetupForm(), {
      type: "accountsRead",
      accounts,
      seedSignedIn: true,
    });
    const adding = run(initialSetupForm(), {
      type: "accountsRead",
      accounts,
      seedSignedIn: false,
    });
    expect(onboarding.accounts.observed).toBe(true);
    expect(adding.accounts.signedIn).toEqual({});
    expect(run(onboarding, { type: "accountsCleared" }).accounts.observed).toBe(false);
  });

  it("keeps an account set aside until a sign-in connects another", () => {
    const account = (credentialId: string) =>
      ({
        credentialId,
        enabled: true,
        pendingSave: false,
        provider: "chatgpt-codex",
        accountRef: null,
      }) as unknown as ProviderAccountView;
    const read = (...ids: string[]) =>
      ({
        type: "accountsRead",
        accounts: ids.map(account),
        seedSignedIn: true,
      }) as const;
    const connected = run(initialSetupForm(), read("c-1"));
    expect(connected.accounts.signedIn).toEqual({
      openai: { credentialId: "c-1", accountRef: null },
    });
    const aside = run(
      connected,
      { type: "signedOut", provider: "openai" },
      read("c-1"),
    );
    expect(aside.accounts.signedIn).toEqual({});
    const again = run(
      aside,
      {
        type: "connected",
        provider: "openai",
        account: { credentialId: "c-2", accountRef: "acct-2" },
      },
      read("c-2"),
    );
    expect(again.accounts.signedIn).toEqual({
      openai: { credentialId: "c-2", accountRef: null },
    });
    expect(again.accounts.switching).toEqual({});
  });
});
