import { openExternalUrl } from "../../lib/externalLinks";

export const PROVIDER_LOGIN_EVENT = {
  openai: "desktop://codex-login-url",
  anthropic: "desktop://claude-login-url",
  grok: "desktop://grok-login-url",
} as const;

export type OauthProvider = keyof typeof PROVIDER_LOGIN_EVENT;

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
    void openExternalUrl(url);
  });
}
