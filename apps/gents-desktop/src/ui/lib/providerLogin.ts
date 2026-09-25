export const PROVIDER_LOGIN_EVENT = {
  openai: "desktop://codex-login-url",
  anthropic: "desktop://claude-login-url",
  grok: "desktop://grok-login-url",
} as const;

export type OauthProvider = keyof typeof PROVIDER_LOGIN_EVENT;

export const PROVIDER_CREDENTIAL_KIND: Record<OauthProvider, string> = {
  openai: "chatgpt-codex",
  anthropic: "claude-subscription",
  grok: "xai-oauth",
};

export async function watchProviderLoginUrl(
  provider: OauthProvider,
  onUrl: (url: string) => void,
): Promise<() => void> {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    return () => {};
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ url: string }>(PROVIDER_LOGIN_EVENT[provider], (event) => {
    const url = event.payload?.url;
    if (!url) return;
    onUrl(url);
    // Native login owns the automatic browser launch. This event only exposes
    // the URL for the user's explicit "Open browser" fallback.
  });
}

/** Bridge code for a completed sign-in whose credential could not be saved. */
export const CREDENTIAL_NOT_SAVED = "credentialNotSaved";

export function bridgeErrorCode(cause: unknown): string | null {
  if (!cause || typeof cause !== "object") return null;
  const code = (cause as { code?: unknown }).code;
  return typeof code === "string" ? code : null;
}

const INTERNAL_DETAIL = /https?:\/\/|graphql|\/api\/v\d/i;

/**
 * User-facing text for a setup failure. Bridge setup commands already return
 * user-facing messages; anything that still carries an endpoint URL or an
 * internal GraphQL error is replaced, and the detail goes to the console log.
 */
export function setupErrorMessage(
  cause: unknown,
  fallback = "Something went wrong. Try again.",
): string {
  const message = cause instanceof Error ? cause.message : String(cause ?? "");
  if (!message.trim()) return fallback;
  if (INTERNAL_DETAIL.test(message)) {
    console.warn("setup error detail", cause);
    return bridgeErrorCode(cause) === "endpointUnreachable"
      ? "The agent is not reachable. Make sure it is running, then try again."
      : fallback;
  }
  return message;
}
