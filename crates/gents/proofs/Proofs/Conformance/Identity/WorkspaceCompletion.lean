import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceCompletion

open Lean Identity Identity.WorkspaceAndFileRequests

structure CompletionAccessInput where
  actor : String
  deriving ToJson

structure CompletionAccessOutput where
  read : Bool
  deriving ToJson

@[conformance_family]
def access : Family CompletionAccessInput CompletionAccessOutput where
  inputs := ["agent", "other"].map fun actor => { actor }
  model := fun i =>
    { read := Household.allowed 5 workspaceFixture i.actor "agent_children" "workspace" "read" }

end Conformance.WorkspaceCompletion
