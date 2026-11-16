import Proofs.Runtime.RuntimeReconcile.StartupReadiness
import Proofs.Conformance.Family

namespace Conformance.StartupReadiness
open Lean RuntimeReconcile.StartupReadiness

deriving instance ToJson for GenerationKind, GenerationStep, GenerationObservation

structure GenerationTrace where
  steps : List GenerationStep
  deriving ToJson

@[conformance_family]
def generationTrace : Family GenerationTrace (List GenerationObservation) where
  inputs :=
    [ { steps := [⟨.register, 1⟩, ⟨.register, 2⟩, ⟨.retire, 2⟩, ⟨.retire, 1⟩] }
    , { steps := [⟨.register, 1⟩, ⟨.register, 2⟩, ⟨.retire, 1⟩, ⟨.retire, 2⟩] }
    , { steps := [⟨.register, 1⟩, ⟨.retire, 1⟩] }
    , { steps := [⟨.register, 1⟩, ⟨.register, 2⟩, ⟨.retire, 2⟩] }
    , { steps := [⟨.register, 2⟩, ⟨.retire, 1⟩] }
    , { steps := [⟨.retire, 1⟩] } ]
  model := fun trace => observeGenerationTrace { generations := ∅ } trace.steps

end Conformance.StartupReadiness
