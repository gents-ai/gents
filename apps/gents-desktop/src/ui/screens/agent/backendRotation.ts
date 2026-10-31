import type {
  BackendAuth,
  InferenceBackendView,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

/** Only the Claude subscription provider can rotate over an account pool. */
export const POOL_KIND = "ClaudeCliSubscription";

export const isPool = (backend: Pick<InferenceBackendView, "authKind">) =>
  backend.authKind === "node_oauth_pool";

export const canPool = (backend: Pick<InferenceBackendView, "providerKind">) =>
  backend.providerKind === POOL_KIND;

/** The enabled accounts a pool walks, in the order the node lists them. */
export function poolAccounts(
  accounts: readonly ProviderAccountView[],
  provider: string,
): ProviderAccountView[] {
  return accounts.filter((a) => a.provider === provider && a.enabled && !a.pendingSave);
}

/**
 * The `auth` a field save sends, or `undefined` to leave it as stored. A pool is
 * set only through its own control, so editing another field never rewrites it.
 */
export function authChange(
  backend: Pick<InferenceBackendView, "authKind" | "accountRef" | "apiKeyConfigured">,
  savedKind: string,
  next: { providerKind: string; apiKey: string; apiKeyEnvVar: string },
  subscription: (kind: string) => boolean,
): BackendAuth | undefined {
  if (subscription(next.providerKind)) {
    if (isPool(backend) && next.providerKind === savedKind) return undefined;
    return {
      kind: "node_oauth",
      ...(backend.accountRef ? { account_ref: backend.accountRef } : {}),
    };
  }
  if (next.apiKey.trim()) return { kind: "api_key", key: next.apiKey };
  if (next.apiKeyEnvVar.trim())
    return { kind: "environment", variable: next.apiKeyEnvVar.trim() };
  if (!backend.apiKeyConfigured || subscription(savedKind))
    return { kind: "unauthenticated" };
  return undefined;
}
