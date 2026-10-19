import type { OauthProvider } from "./transport.js";
export type { OauthProvider };

export const PROVIDER_CREDENTIAL_KIND: Record<OauthProvider, string> = {
  openai: "chatgpt-codex",
  anthropic: "claude-subscription",
  grok: "xai-oauth",
};
