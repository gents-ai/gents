/* Which mark stands for a backend. The provider kind decides for the
   subscription and router kinds; an OpenAI-compatible endpoint gets a
   mark only when its URL names the company (an openai.com host, an
   ollama host or path); a bare local port says nothing, so no mark. The
   files are the companies' own monochrome SVGs in public/logos. */
export function providerLogo(kind: string | null | undefined, endpoint?: string | null) {
  switch (kind) {
    case 'ChatGptCodex':
      return '/logos/codex.svg'
    case 'XaiGrokOAuth':
      return '/logos/grok.svg'
    case 'OpenRouter':
      return '/logos/openrouter.svg'
    case 'ClaudeCliSubscription':
      return '/logos/claude.svg'
    default: {
      const url = endpoint ?? ''
      if (/openai\.com/i.test(url)) return '/logos/openai.svg'
      if (/ollama/i.test(url)) return '/logos/ollama.svg'
      return null
    }
  }
}
