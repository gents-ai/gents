export function providerLogo(
  kind: string | null | undefined,
  endpoint?: string | null,
) {
  switch (kind) {
    case "ChatGptCodex":
      return "/logos/codex.svg";
    case "XaiGrokOAuth":
      return "/logos/grok.svg";
    case "OpenRouter":
      return "/logos/openrouter.svg";
    case "ClaudeCliSubscription":
    case "AnthropicApiKey":
      return "/logos/claude.svg";
    default: {
      const url = endpoint ?? "";
      if (/openai\.com/i.test(url)) return "/logos/openai.svg";
      if (/ollama/i.test(url)) return "/logos/ollama.svg";
      return null;
    }
  }
}
