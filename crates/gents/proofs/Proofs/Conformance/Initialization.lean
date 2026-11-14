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
  concurrent : Option Int
  deriving ToJson

structure SeedPlan where
  model : Option String
  enabled : Bool
  concurrent : Option Int
  effectiveConcurrent : Nat
  contextOverride : Option Int
  outputOverride : Option Int
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
            concurrent := i.concurrent }).map fun plan =>
      { model := plan.model, enabled := plan.enabled
        concurrent := plan.concurrent
        effectiveConcurrent := plan.effectiveConcurrent
        contextOverride := plan.contextOverride
        outputOverride := plan.outputOverride
        recordsProbe := plan.recordsProbe
        unselectedModelReason := unselectedModelReason plan.enabled plan.model
        enabledUnselectedModelReason := unselectedModelReason true plan.model }

end Conformance.Initialization
