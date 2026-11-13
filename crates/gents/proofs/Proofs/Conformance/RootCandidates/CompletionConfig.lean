import Proofs.Runtime.CompletionConfig
import Proofs.Conformance.Family

namespace Runtime.CompletionConfig

@[conformance_family]
def loopConfigForRequest : Conformance.Family Input (Option Observation) :=
  ⟨["interactive", "scheduled", "unknown"].flatMap (fun origin =>
    [none, some (-1), some 0, some 2].flatMap fun interactive =>
      [none, some [100, 0, -1, 200]].flatMap fun delays =>
        [none, some 1893456000].map fun deadline =>
          ({ origin, transport := none, interactive, resample := none, repair := none, delays,
             hops := 2, deadline } : Input)) ++
    ["OpenAiCompatible", "OpenRouter", "ChatGptCodex", "AnthropicApiKey"].flatMap (fun provider =>
      [false, true].flatMap fun responses =>
        [false, true].flatMap fun sampled =>
          [("session-456", "request-123"), ("  ", "request-123"), ("", "")].flatMap fun (session, request) =>
            [none, some 0, some 4096].map fun budget =>
              { origin := "interactive", transport := none, interactive := none, resample := none,
                repair := none, delays := none, hops := 0, deadline := none, provider, responses,
                temperature := if sampled then some 1 else none,
                topK := if sampled then some 40 else none,
                maxTokens := if sampled then some 1024 else none,
                session, request, budget }), resolve⟩

end Runtime.CompletionConfig
