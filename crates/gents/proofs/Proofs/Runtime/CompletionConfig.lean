import Lean
import Proofs.Runtime.CompletionParameters
import Proofs.Runtime.ReasoningCapabilities
import Proofs.Shared.CompletionParametersMerge

namespace Runtime.CompletionConfig
open Lean

structure Input where
  origin : String
  transport : Option Int
  interactive : Option Int
  resample : Option Int
  repair : Option Bool
  delays : Option (List Int)
  hops : Nat
  deadline : Option Int
  provider : String := "OpenAiCompatible"
  responses : Bool := false
  temperature : Option Nat := none
  topK : Option Nat := none
  maxTokens : Option Nat := none
  session : String := "session-456"
  request : String := "request-123"
  budget : Option Nat := none
  deriving ToJson

structure Observation where
  delays : List Nat
  resample : Nat
  empty : Nat
  repair : Bool
  hops : Nat
  deadline : Option Int
  budget : Option Nat
  params : Option String
  deriving ToJson

def nonnegative (value : Option Int) : Option Nat :=
  value.bind fun value => if value < 0 then none else some value.toNat

def openAiKinds : List String := ["OpenAiCompatible", "OpenRouter", "ChatGptCodex", "XaiGrokOAuth"]

/-- OpenRouter must honour every sent parameter rather than silently drop one.
The modeled backend endpoint is not xAI's, so a generic Responses backend adds
no storage parameters. -/
def providerParams (provider : String) : Option String :=
  if provider == "OpenRouter" then some "{\"provider\":{\"require_parameters\":true}}" else none

/-- Temperature and the output cap are top-level request fields; only the other
sampling knobs ride in additional parameters. -/
def samplingParams (input : Input) : Option String :=
  input.topK.map fun topK => (Json.mkObj [("top_k", toJson topK)]).compress

/-- Agent parameters merge reasoning, provider and sampling sets, then the
request's session-scoped cache parameters; an unadvertised profile sends no
reasoning effort, and only Messages providers carry advertised capabilities. -/
def params (input : Input) : Option String :=
  let merge (left right : Option String) := CompletionParameters.Merge.merge ⟨left, right⟩
  let agent := merge
    (merge (CompletionParameters.reasoning ⟨input.provider, input.responses, none⟩) (providerParams input.provider))
    (samplingParams input)
  let agent := ReasoningCapabilities.authoritative ⟨!openAiKinds.contains input.provider, agent, none⟩
  merge agent (CompletionParameters.cache ⟨input.provider, input.session, input.request⟩)

/-- Scheduled work has a longer transport ladder; an interactive override
only affects interactive work. Positive authored delays survive validation,
and resizing repeats the final delay instead of inventing a new cadence. -/
def resolve (input : Input) : Option Observation := do
  if input.origin != "interactive" && input.origin != "scheduled" then none else do
    let interactive := input.origin == "interactive"
    let defaultDelays := if interactive then [2000] else [5000, 30000, 120000]
    let delays := input.delays.map (fun values => values.filterMap fun value =>
      if value > 0 then some value.toNat else none)
    let delays := match delays with | some (first :: rest) => first :: rest | _ => defaultDelays
    let maximum := nonnegative (if interactive then input.interactive.orElse (fun _ => input.transport) else input.transport)
    let delays := match maximum with
      | none => delays
      | some maximum => (List.range maximum).map fun index => (delays[index]?).getD (delays.getLast!)
    let resample := (nonnegative input.resample).getD (if interactive then 0 else 1)
    let empty := (nonnegative input.resample).getD 1
    some ⟨delays, resample, empty, input.repair.getD true, input.hops, input.deadline,
      input.budget, params input⟩

end Runtime.CompletionConfig
