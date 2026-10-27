import Proofs.Configuration.Initialization
import Lean

namespace Conformance.Initialization

open Configuration.Initialization Lean

def localDefault : String := "google/gemma-4-12B-it-qat-q4_0-gguf"

def inputs : List Input :=
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

def casesJson : String := (toJson (inputs.map fun input =>
  Json.mkObj [
    ("engineer", toJson input.engineer),
    ("requested_model", toJson input.requestedModel),
    ("local_default", toJson input.localDefault),
    ("preset", toJson input.preset),
    ("preset_default", toJson input.presetDefault),
    ("concurrent", toJson input.concurrent),
    ("expected", (seed input).map (fun plan => Json.mkObj [
      ("model", toJson plan.model), ("enabled", toJson plan.enabled),
      ("concurrent", toJson plan.concurrent),
      ("effective_concurrent", toJson plan.effectiveConcurrent),
      ("context_override", toJson plan.contextOverride),
      ("output_override", toJson plan.outputOverride),
      ("records_probe", toJson plan.recordsProbe)]) |>.getD Json.null)])).compress

end Conformance.Initialization
