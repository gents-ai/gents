import Lean
import Proofs.Shared.TextNormalization

namespace Runtime.CompletionParameters
open Lean

structure ReasoningInput where
  provider : String
  responses : Bool
  effort : Option String
  deriving ToJson

/-- The local chat server accepts chat_template_kwargs; Responses, OpenRouter
and Codex accept reasoning.effort; Messages accepts output_config.effort.
Codex supplies medium when its profile has no effort. Grok rejects effort. -/
def reasoning (input : ReasoningInput) : Option String :=
  let effort := input.effort.orElse (fun _ =>
    if input.provider == "ChatGptCodex" then some "medium" else none)
  effort.bind fun effort =>
    if input.provider == "XaiGrokOAuth" then none
    else if input.provider == "OpenAiCompatible" && !input.responses then
      some (Json.mkObj [("chat_template_kwargs", Json.mkObj
        ([("enable_thinking", toJson (effort != "none"))] ++
          if effort == "none" then [] else [("reasoning_effort", toJson effort)]))]).compress
    else if ["OpenAiCompatible", "OpenRouter", "ChatGptCodex"].contains input.provider then
      some (Json.mkObj [("reasoning", Json.mkObj [("effort", toJson effort)])]).compress
    else some (Json.mkObj [("output_config", Json.mkObj [("effort", toJson effort)])]).compress

structure CacheInput where
  provider : String
  session : String
  request : String
  deriving ToJson

def normalized (value : String) : Option String :=
  let value := TextNormalization.canonicalObjective value
  if value.isEmpty then none else some value

/-- A session owns cache affinity across requests. Only providers with an
explicit session parameter receive one; a Claude subscription never invents
session affinity from a request ID. -/
def cache (input : CacheInput) : Option String := do
  let scope ← if input.provider == "ClaudeCliSubscription" then normalized input.session
    else (normalized input.session).orElse (fun _ => normalized input.request)
  let key ← match input.provider with
    | "OpenAiCompatible" => some "user"
    | "ChatGptCodex" => some "prompt_cache_key"
    | "ClaudeCliSubscription" => some "_gents_session_key"
    | _ => none
  some (Json.mkObj [(key, toJson scope)]).compress

end Runtime.CompletionParameters
