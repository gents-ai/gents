import Proofs.Tools.SelfConfig.StructuredConfig
import Proofs.Conformance.Family

namespace Conformance.SelfConfigStructured

open Lean SelfConfig SelfConfig.StructuredConfig

structure TraceInput where
  prompt : String
  deriving ToJson

structure TraceOutput where
  previewCommitted : Bool
  previewMutationEntered : Bool
  unchangedAfterPreview : Bool
  appliedMutationEntered : Bool
  storedPromptMatches : Bool
  foreignRefused : Bool
  unchangedAfterRefusal : Bool
  deriving ToJson

@[conformance_family]
def trace : Family TraceInput TraceOutput where
  inputs := [{ prompt := "Quoted \"text\"\nActual newline; literal \\n; Unicode λ; {{ doc.message }}" }]
  model := fun i =>
    let previewed := previewStore initialStore (promptPatch i.prompt)
    let applied := commitStore previewed (promptPatch i.prompt)
    let refused := commitStore applied.1 foreignPatch
    { previewCommitted := committed .preview
      previewMutationEntered := mutationEntered .preview
      unchangedAfterPreview := fieldsEqual previewed initialStore
      appliedMutationEntered := mutationEntered .apply
      storedPromptMatches := decide (storedPrompt applied.1 = some i.prompt)
      foreignRefused := !refused.2
      unchangedAfterRefusal := fieldsEqual refused.1 applied.1 }

end Conformance.SelfConfigStructured
