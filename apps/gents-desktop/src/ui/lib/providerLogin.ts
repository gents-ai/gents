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
