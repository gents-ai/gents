/* Which inference backends exist, which run on a subscription account,
   and what counts as a healthy one. */
import type {
  InferenceAuthMethod,
  InferenceProviderId,
} from "@source-inc/gents-desktop-client";
import { PROVIDER_CREDENTIAL_KIND } from "@/lib/providerLogin";

export const isSubscriptionKind = (kind: string) =>
  kind === "ChatGptCodex" ||
  kind === "XaiGrokOAuth" ||
  kind === "ClaudeCliSubscription";

export const healthy = (status: string | null) =>
  status === "healthy" || status === "ok";

export const KINDS = [
  { value: "OpenAiCompatible", label: "OpenAI compatible" },
  { value: "OpenRouter", label: "OpenRouter" },
  { value: "ChatGptCodex", label: "ChatGPT / Codex (subscription)" },
  { value: "XaiGrokOAuth", label: "Grok (subscription)" },
  { value: "ClaudeCliSubscription", label: "Anthropic / Claude (subscription)" },
  { value: "AnthropicApiKey", label: "Anthropic API key" },
];

/* subscription kinds, and the provider name their account carries */
export const SUBSCRIPTION: Record<
  string,
  {
    provider: string;
    providerId: InferenceProviderId;
    authMethod: InferenceAuthMethod;
    title: string;
    note: string;
    login: "codex" | "grok" | "claude";
  }
> = {
  ChatGptCodex: {
    provider: PROVIDER_CREDENTIAL_KIND.openai,
    providerId: "openai",
    authMethod: "chat_gpt_oauth",
    title: "ChatGPT / Codex",
    note: "Use an eligible ChatGPT subscription for Codex inference.",
    login: "codex",
  },
  XaiGrokOAuth: {
    provider: PROVIDER_CREDENTIAL_KIND.grok,
    providerId: "grok",
    authMethod: "grok_oauth",
    title: "Grok / xAI",
    note: "Use SuperGrok or an eligible X Premium+ subscription.",
    login: "grok",
  },
  ClaudeCliSubscription: {
    provider: PROVIDER_CREDENTIAL_KIND.anthropic,
    providerId: "anthropic",
    authMethod: "claude_oauth",
    title: "Anthropic / Claude",
    note: "Use a Claude Pro or Max subscription.",
    login: "claude",
  },
};
