import { describe, expect, it } from "vitest";
import {
  authChange,
  canPool,
  movedCredential,
  isPool,
  poolAccounts,
} from "../src/ui/screens/agent/backendRotation";
import { isSubscriptionKind } from "../src/ui/screens/agent/inferenceKinds";

const backend = (authKind: string, accountRef: string | null = null) => ({
  authKind,
  accountRef,
  apiKeyConfigured: false,
});
const ENDPOINT = "https://saved.example";
const saved = (providerKind = "ClaudeCliSubscription") => ({
  providerKind,
  endpoint: ENDPOINT,
});
const fields = (providerKind = "ClaudeCliSubscription") => ({
  providerKind,
  endpoint: ENDPOINT,
  apiKey: "",
  apiKeyEnvVar: "",
});

describe("a field save and the account pool", () => {
  it("leaves a pool backend's auth as stored", () => {
    expect(
      authChange(
        backend("node_oauth_pool"),
        saved("ClaudeCliSubscription"),
        fields(),
        isSubscriptionKind,
      ),
    ).toBeUndefined();
  });

  it("still writes a single-account backend's reference", () => {
    expect(
      authChange(
        backend("node_oauth", "acct-2"),
        saved("ClaudeCliSubscription"),
        fields(),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "node_oauth", account_ref: "acct-2" });
    expect(
      authChange(
        backend("node_oauth"),
        saved("ClaudeCliSubscription"),
        fields(),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "node_oauth" });
  });

  it("writes a plain subscription sign-in when a pool changes provider", () => {
    expect(
      authChange(
        backend("node_oauth_pool"),
        saved("ClaudeCliSubscription"),
        fields("ChatGptCodex"),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "node_oauth" });
  });

  it("keeps the key and environment choices for non-subscription backends", () => {
    const keyed = { authKind: "api_key", accountRef: null, apiKeyConfigured: true };
    expect(
      authChange(
        keyed,
        saved("OpenAiCompatible"),
        { ...fields("OpenAiCompatible"), apiKey: "sk-x" },
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "api_key", key: "sk-x" });
    expect(
      authChange(
        keyed,
        saved("OpenAiCompatible"),
        { ...fields("OpenAiCompatible"), apiKeyEnvVar: " KEY " },
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "environment", variable: "KEY" });
    expect(
      authChange(
        keyed,
        saved("OpenAiCompatible"),
        fields("OpenAiCompatible"),
        isSubscriptionKind,
      ),
    ).toBeUndefined();
    expect(
      authChange(
        backend("node_oauth"),
        saved("ClaudeCliSubscription"),
        fields("OpenAiCompatible"),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "unauthenticated" });
  });
});

describe("a field save and credentials the editor has no field for", () => {
  it("keeps a cloud backend's credentials when nothing about them was chosen", () => {
    for (const authKind of ["aws_credentials", "google_credentials"])
      expect(
        authChange(
          backend(authKind),
          saved("ClaudeBedrock"),
          fields("ClaudeBedrock"),
          isSubscriptionKind,
        ),
      ).toBeUndefined();
  });

  it("keeps a gateway backend's sign-in", () => {
    expect(
      authChange(
        backend("node_oauth"),
        saved("ClaudeGateway"),
        fields("ClaudeGateway"),
        isSubscriptionKind,
      ),
    ).toBeUndefined();
  });

  it("still writes a key typed for it, and clears a cleared variable", () => {
    expect(
      authChange(
        backend("aws_credentials"),
        saved("ClaudeBedrock"),
        { ...fields("ClaudeBedrock"), apiKey: "sk-x" },
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "api_key", key: "sk-x" });
    expect(
      authChange(
        { authKind: "environment", accountRef: null, apiKeyConfigured: false },
        saved("OpenAiCompatible"),
        fields("OpenAiCompatible"),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "unauthenticated" });
  });
});

describe("a field save that moves a kept credential's endpoint", () => {
  const moved = (providerKind: string) => ({
    ...fields(providerKind),
    endpoint: "https://elsewhere.example",
  });

  it("is refused for a gateway sign-in and for cloud credentials", () => {
    for (const [authKind, kind] of [
      ["node_oauth", "ClaudeGateway"],
      ["aws_credentials", "ClaudeBedrock"],
      ["google_credentials", "ClaudeVertex"],
    ]) {
      expect(
        movedCredential(
          backend(authKind),
          saved(kind),
          moved(kind),
          isSubscriptionKind,
        ),
      ).toBeTruthy();
      expect(() =>
        authChange(backend(authKind), saved(kind), moved(kind), isSubscriptionKind),
      ).toThrow(/saved endpoint/);
    }
  });

  it("goes through when a key is chosen for the new endpoint", () => {
    const next = { ...moved("ClaudeGateway"), apiKey: "sk-x" };
    expect(
      movedCredential(
        backend("node_oauth"),
        saved("ClaudeGateway"),
        next,
        isSubscriptionKind,
      ),
    ).toBeUndefined();
    expect(
      authChange(
        backend("node_oauth"),
        saved("ClaudeGateway"),
        next,
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "api_key", key: "sk-x" });
  });

  it("leaves a stored API key and a subscription as they were", () => {
    expect(
      movedCredential(
        { authKind: "api_key" },
        saved("OpenAiCompatible"),
        moved("OpenAiCompatible"),
        isSubscriptionKind,
      ),
    ).toBeUndefined();
    expect(
      movedCredential(
        backend("node_oauth"),
        saved(),
        moved("ClaudeCliSubscription"),
        isSubscriptionKind,
      ),
    ).toBeUndefined();
  });
});

describe("the account pool view", () => {
  const account = (credentialId: string, fieldsOf: object = {}) => ({
    credentialId,
    nodeDid: "did:test",
    provider: "claude-subscription",
    accountId: null,
    planType: null,
    accessTokenExpiresAt: "2099-01-01T00:00:00Z",
    lastRefresh: null,
    enabled: true,
    pendingSave: false,
    accountRef: null,
    label: `label-${credentialId}`,
    ...fieldsOf,
  });

  it("lists enabled stored accounts of the provider in the given order", () => {
    const rows = [
      account("b"),
      account("off", { enabled: false }),
      account("grok", { provider: "xai-oauth" }),
      account("held", { pendingSave: true }),
      account("a"),
    ];
    expect(
      poolAccounts(rows, "claude-subscription").map((a) => a.credentialId),
    ).toEqual(["b", "a"]);
  });

  it("recognises a pool and the one provider that can have it", () => {
    expect(isPool({ authKind: "node_oauth_pool" })).toBe(true);
    expect(isPool({ authKind: "node_oauth" })).toBe(false);
    expect(isPool({ authKind: null })).toBe(false);
    expect(canPool({ providerKind: "ClaudeCliSubscription" })).toBe(true);
    expect(canPool({ providerKind: "ChatGptCodex" })).toBe(false);
  });
});
