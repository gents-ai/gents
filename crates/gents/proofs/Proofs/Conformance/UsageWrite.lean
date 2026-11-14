import Proofs.Inference.UsageBinding
import Proofs.Conformance.Family

namespace Conformance.UsageWrite

open Lean

structure BindingInput where
  observed : Option String
  current : String
  deriving ToJson

@[conformance_family]
def binding : Conformance.Family BindingInput Bool where
  inputs :=
    ([none, some "row-a", some "row-b"] : List (Option String)).flatMap fun observed =>
      ["row-a", "row-b"].map fun current => { observed, current }
  model := fun i => UsageBinding.lands i.observed i.current

end Conformance.UsageWrite
