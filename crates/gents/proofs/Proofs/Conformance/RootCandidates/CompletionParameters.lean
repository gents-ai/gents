import Proofs.Runtime.CompletionParameters
import Proofs.Conformance.Family

namespace Runtime.CompletionParameters
open Lean Conformance

@[conformance_family]
def reasoningProfileParams : Family ReasoningInput (Option String) :=
  ⟨["OpenAiCompatible", "OpenRouter", "ChatGptCodex", "XaiGrokOAuth",
    "ClaudeCliSubscription", "AnthropicApiKey", "ClaudeBedrock", "ClaudeVertex",
    "ClaudeFoundry", "ClaudeAnthropicAws", "ClaudeMantle", "ClaudeGoogleCloud", "ClaudeGateway"].flatMap
    (fun provider => [false, true].flatMap fun responses =>
      [none, some "none", some "minimal", some "low", some "medium", some "high", some "xhigh", some "max", some "ultra"].map
        fun effort => ⟨provider, responses, effort⟩), reasoning⟩

private def cacheInputs : List CacheInput :=
  ["OpenAiCompatible", "ChatGptCodex", "ClaudeCliSubscription", "OpenRouter", "AnthropicApiKey", "XaiGrokOAuth"].flatMap
    fun provider => ["", "  ", " session-456 "].flatMap fun session =>
      ["", "request-123", "next-request"].map fun request => ⟨provider, session, request⟩

@[conformance_family]
def requestAdditionalParams : Family CacheInput (Option String) := ⟨cacheInputs, cache⟩

@[conformance_family]
def openaiCacheScopeParams : Family CacheInput (Option String) :=
  ⟨cacheInputs.filter (·.provider == "OpenAiCompatible"), cache⟩

end Runtime.CompletionParameters
