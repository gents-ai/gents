import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceCallbackSelection

open Lean Identity Identity.WorkspaceAndFileRequests

structure Input where
  actor : String
  deriving ToJson

structure Output where
  before : Bool
  afterRevocation : Bool
  afterDisabled : Bool
  afterReenabled : Bool
  deriving ToJson

private def readsTarget (state : Household.State) (actor : String) : Bool :=
  Household.allowed 8 state actor "agents" "target" "read"

@[conformance_family]
def visibility : Family Input Output where
  inputs := ["user", "creator", "other"].map fun actor => { actor }
  model := fun i =>
    { before := callbackSelectedAgentPermission i.actor
      afterRevocation := readsTarget callbackSelectedAgentRevoked i.actor
      afterDisabled := readsTarget (callbackSelectionState false) i.actor
      afterReenabled := readsTarget (callbackSelectionState true) i.actor }

end Conformance.WorkspaceCallbackSelection
