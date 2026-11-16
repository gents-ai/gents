import Proofs.Session.Workspace.PathCapability
import Proofs.Conformance.Family

namespace Conformance.WorkspaceDirtyTrunk

open Lean Workspace.PathCapability

structure IntegrateInput where
  dirtyPath : Option String
  deltaPath : String
  deriving ToJson

structure IntegrateOutput where
  refused : Bool
  trunkEffects : Nat
  overlapRefusal : Bool
  dirtyPreserved : Bool
  deriving ToJson

@[conformance_family]
def integrate : Family IntegrateInput IntegrateOutput where
  inputs :=
    [{ dirtyPath := some "README.md", deltaPath := "README.md" },
     { dirtyPath := some "notes.md", deltaPath := "README.md" },
     { dirtyPath := none, deltaPath := "README.md" }]
  model := fun i =>
    let e := dirtyEvidence i.dirtyPath i.deltaPath
    let outcome := execute dirtySealed .integrate e
    { refused := outcome.2 == .denied
      trunkEffects := outcome.1.trunkEffects
      overlapRefusal := overlaps e && outcome.2 == .denied
      dirtyPreserved := !(overlaps e && outcome.2 == .accepted) }

end Conformance.WorkspaceDirtyTrunk
