import type { OauthProvider } from "@source-inc/gents-desktop-client";

export type { OauthProvider };

export const PROVIDER_CREDENTIAL_KIND: Record<OauthProvider, string> = {
  openai: "chatgpt-codex",
  anthropic: "claude-subscription",
  grok: "xai-oauth",
};

/** Bridge code for a completed sign-in whose credential could not be saved. */
export const CREDENTIAL_NOT_SAVED = "credentialNotSaved";
