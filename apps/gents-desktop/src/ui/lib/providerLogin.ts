import type { OauthProvider } from "@source-inc/gents-desktop-client";

export type { OauthProvider };

export const PROVIDER_CREDENTIAL_KIND: Record<OauthProvider, string> = {
  openai: "chatgpt-codex",
  anthropic: "claude-subscription",
  grok: "xai-oauth",
};

export const CREDENTIAL_NOT_SAVED = "credentialNotSaved";
