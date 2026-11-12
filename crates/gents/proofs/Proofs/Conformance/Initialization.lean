import Proofs.Documents.Configuration.Initialization
import Proofs.Conformance.Family

namespace Conformance.Initialization

open Configuration.Initialization Lean

def localDefault : String := "google/gemma-4-12B-it-qat-q4_0-gguf"

structure SeedInput where
  engineer : Bool
  requestedModel : Option String
  preset : Option String
  presetDefault : Option String
  localDefault : String
  concurrent : Option Nat
  deriving ToJson

/-- Seeding never pins a context or output limit, so those overrides are always
absent; the wire carries concurrency as a natural number because authored
concurrency is never negative. -/
structure SeedPlan where
  model : Option String
  enabled : Bool
  concurrent : Option Nat
  effectiveConcurrent : Nat
  contextOverride : Option Nat
  outputOverride : Option Nat
  recordsProbe : Bool
  unselectedModelReason : Option String
  enabledUnselectedModelReason : Option String
  deriving ToJson

def seedInputs : List SeedInput :=
  let defaults :=
    [false, true].flatMap fun engineer =>
      [none, some "model-a", some "  model-b  ", some " "].flatMap fun requested =>
        [none, some 1, some 4, some 0].map fun concurrent =>
          {engineer, requestedModel := requested, preset := none, presetDefault := none,
            localDefault, concurrent}
  defaults ++ ["llama-cpp", "openai"].flatMap fun preset =>
    [none, some "selected-model"].map fun requested =>
      {engineer := true, requestedModel := requested, preset := some preset,
        presetDefault := if preset == "llama-cpp" then some localDefault else none,
        localDefault, concurrent := none}

@[conformance_family]
def initialInference : Conformance.Family SeedInput (Option SeedPlan) where
  inputs := seedInputs
  model := fun i =>
    (seed { engineer := i.engineer, requestedModel := i.requestedModel, preset := i.preset
            presetDefault := i.presetDefault, localDefault := i.localDefault
            concurrent := i.concurrent.map fun n => (n : Int) }).map fun plan =>
      { model := plan.model, enabled := plan.enabled
        concurrent := plan.concurrent.map Int.toNat
        effectiveConcurrent := plan.effectiveConcurrent
        contextOverride := plan.contextOverride.map Int.toNat
        outputOverride := plan.outputOverride.map Int.toNat
        recordsProbe := plan.recordsProbe
        unselectedModelReason := unselectedModelReason plan.enabled plan.model
        enabledUnselectedModelReason := unselectedModelReason true plan.model }

end Conformance.Initialization
