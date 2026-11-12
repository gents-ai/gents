import Proofs.Session.ToolResultThreading
import Proofs.Conformance.Family

open ToolResultThreading

namespace Conformance.ToolResultThreading

deriving instance Lean.ToJson for Profile, Segment

inductive Tool where
  | pluginImage
  | oversizedText
  deriving DecidableEq, Repr, Lean.ToJson

/-- The plugin answers a small JSON response and one PNG image; the other tool
answers text larger than the tool-result text bound. -/
def toolOutput : Tool → List SourcePart
  | .pluginImage => [.text true, .image]
  | .oversizedText => [.text false]

structure ProjectionInput where
  tool : Tool
  profile : Profile
  persisted : Bool
  imageBytes : Nat
  deriving Lean.ToJson

private def profiles : List Profile :=
  [.openAiChatCompletions, .openAiResponsesNormalized, .openRouterChatCompletions,
   .chatGptCodexResponses, .xaiResponses, .claudeMessages]

/-- Persisted and oversized cases run on the owned loop with the OpenAI chat
profile: the scripted owned fixture cannot persist a Claude provider turn,
which needs a canonical replay coordinate from a real Claude stream. -/
@[conformance_family]
def projection : Conformance.Family ProjectionInput (Option (List (List Segment))) where
  inputs :=
    profiles.map (fun profile => { tool := .pluginImage, profile, persisted := false, imageBytes := 64 }) ++
    [ { tool := .pluginImage, profile := .claudeMessages, persisted := false, imageBytes := 900000 }
    , { tool := .pluginImage, profile := .openAiChatCompletions, persisted := false, imageBytes := 900000 }
    , { tool := .pluginImage, profile := .openAiChatCompletions, persisted := true, imageBytes := 64 }
    , { tool := .pluginImage, profile := .openAiChatCompletions, persisted := true, imageBytes := 400000 }
    , { tool := .oversizedText, profile := .openAiChatCompletions, persisted := false, imageBytes := 0 } ]
  model := fun i =>
    if i.persisted then replayed (toolOutput i.tool) else some (threaded i.profile (toolOutput i.tool))

deriving instance Lean.ToJson for AssistantId

structure AssistantIdInput where
  reportsId : Bool
  toolTurns : Nat
  deriving Lean.ToJson

@[conformance_family]
def assistantIds : Conformance.Family AssistantIdInput (List (List AssistantId)) where
  inputs :=
    [false, true].flatMap fun reportsId =>
      [0, 1, 2].map fun toolTurns => { reportsId, toolTurns }
  model := fun i => threadedAssistantIds i.reportsId i.toolTurns

end Conformance.ToolResultThreading
