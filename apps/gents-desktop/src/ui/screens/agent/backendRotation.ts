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

/* stored credentials the editor has no field for (cloud credentials, and a
   sign-in on a provider that isn't one of the app's subscriptions, such as a
   gateway's): a save of other fields keeps them, since none of the editor's
   choices was made, but only for the endpoint they were set up for, since
   kept they would be sent to whatever host the endpoint names */
const KEPT_AUTH = new Set([
  "aws_credentials",
  "google_credentials",
  "node_oauth",
  "node_oauth_pool",
]);

type AuthFields = {
  providerKind: string;
  endpoint: string;
  apiKey: string;
  apiKeyEnvVar: string;
};

/** Why a save can't keep the backend's stored credentials: the endpoint moved
 *  while nothing about the credentials was chosen. */
export function movedCredential(
  backend: Pick<InferenceBackendView, "authKind">,
  saved: { providerKind: string; endpoint: string },
  next: AuthFields,
  subscription: (kind: string) => boolean,
): string | undefined {
  if (subscription(next.providerKind) || next.apiKey.trim() || next.apiKeyEnvVar.trim())
    return undefined;
  if (
    next.providerKind !== saved.providerKind ||
    !KEPT_AUTH.has(backend.authKind ?? "")
  )
    return undefined;
  if (next.endpoint.trim() === saved.endpoint.trim()) return undefined;
  return backend.authKind === "node_oauth"
    ? "This backend's sign-in is for its saved endpoint. Sign in to the new gateway with gents auth login claude-gateway, or put the endpoint back."
    : "This backend's credentials are for its saved endpoint. Set them up for the new one, or put the endpoint back.";
}

/**
 * The `auth` a field save sends, or `undefined` to leave it as stored. A pool is
 * set only through its own control, so editing another field never rewrites it.
 */
export function authChange(
  backend: Pick<InferenceBackendView, "authKind" | "accountRef" | "apiKeyConfigured">,
  saved: { providerKind: string; endpoint: string },
  next: AuthFields,
  subscription: (kind: string) => boolean,
): BackendAuth | undefined {
  const moved = movedCredential(backend, saved, next, subscription);
  if (moved) throw new Error(moved);
  if (subscription(next.providerKind)) {
    if (isPool(backend) && next.providerKind === saved.providerKind) return undefined;
    return {
      kind: "node_oauth",
      ...(backend.accountRef ? { account_ref: backend.accountRef } : {}),
    };
  }
  if (next.apiKey.trim()) return { kind: "api_key", key: next.apiKey };
  if (next.apiKeyEnvVar.trim())
    return { kind: "environment", variable: next.apiKeyEnvVar.trim() };
  if (next.providerKind === saved.providerKind && KEPT_AUTH.has(backend.authKind ?? ""))
    return undefined;
  if (!backend.apiKeyConfigured || subscription(saved.providerKind))
    return { kind: "unauthenticated" };
  return undefined;
}
