import { describe, expect, it } from "vitest";
import {
  authChange,
  canPool,
  isPool,
  poolAccounts,
} from "../src/ui/screens/agent/backendRotation";
import { isSubscriptionKind } from "../src/ui/screens/agent/inferenceKinds";

const backend = (authKind: string, accountRef: string | null = null) => ({
  authKind,
  accountRef,
  apiKeyConfigured: false,
});
const fields = (providerKind = "ClaudeCliSubscription") => ({
  providerKind,
  apiKey: "",
  apiKeyEnvVar: "",
});

describe("a field save and the account pool", () => {
  it("leaves a pool backend's auth as stored", () => {
    expect(
      authChange(
        backend("node_oauth_pool"),
        "ClaudeCliSubscription",
        fields(),
        isSubscriptionKind,
      ),
    ).toBeUndefined();
  });

  it("still writes a single-account backend's reference", () => {
    expect(
      authChange(
        backend("node_oauth", "acct-2"),
        "ClaudeCliSubscription",
        fields(),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "node_oauth", account_ref: "acct-2" });
    expect(
      authChange(
        backend("node_oauth"),
        "ClaudeCliSubscription",
        fields(),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "node_oauth" });
  });

  it("writes a plain subscription sign-in when a pool changes provider", () => {
    expect(
      authChange(
        backend("node_oauth_pool"),
        "ClaudeCliSubscription",
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
        "OpenAiCompatible",
        { ...fields("OpenAiCompatible"), apiKey: "sk-x" },
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "api_key", key: "sk-x" });
    expect(
      authChange(
        keyed,
        "OpenAiCompatible",
        { ...fields("OpenAiCompatible"), apiKeyEnvVar: " KEY " },
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "environment", variable: "KEY" });
    expect(
      authChange(
        keyed,
        "OpenAiCompatible",
        fields("OpenAiCompatible"),
        isSubscriptionKind,
      ),
    ).toBeUndefined();
    expect(
      authChange(
        backend("node_oauth"),
        "ClaudeCliSubscription",
        fields("OpenAiCompatible"),
        isSubscriptionKind,
      ),
    ).toEqual({ kind: "unauthenticated" });
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
