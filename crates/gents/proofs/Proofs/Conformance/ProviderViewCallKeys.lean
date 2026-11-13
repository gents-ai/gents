import Proofs.Session.Compaction.ProviderView
import Proofs.Conformance.Family

namespace Conformance.ProviderViewCallKeys

open Lean Compaction PromptAssembly

structure StepWire where
  kind : String
  callIds : List Nat
  deriving ToJson

structure Scenario where
  steps : List StepWire
  deriving ToJson

structure ViewOutput where
  identity : Bool
  callCount : Nat
  resultCount : Nat
  deriving ToJson

def StepWire.toModel (step : StepWire) : ProviderViewStep :=
  match step.kind with
  | "calls" => .calls step.callIds
  | "result" => .result (step.callIds.headD 0)
  | _ => .ordinary

@[conformance_family]
def sanitize : Family Scenario ViewOutput where
  inputs :=
    [ { steps := [⟨"calls", [1]⟩, ⟨"result", [1]⟩, ⟨"calls", [1]⟩, ⟨"result", [1]⟩] }
    , { steps := [⟨"calls", [1]⟩, ⟨"result", [1]⟩, ⟨"calls", [1]⟩] }
    , { steps := [⟨"calls", [1]⟩, ⟨"result", [1]⟩, ⟨"calls", [1]⟩, ⟨"result", [1]⟩,
        ⟨"calls", [1]⟩] }
    , { steps := [] } ]
  model := fun i =>
    let rows := callScopeRows (i.steps.map StepWire.toModel)
    { identity := decide (sanitizeTurn rows = rows)
      callCount := callCountOf (sanitizeTurn rows)
      resultCount := resultCountOf (sanitizeTurn rows) }

end Conformance.ProviderViewCallKeys
