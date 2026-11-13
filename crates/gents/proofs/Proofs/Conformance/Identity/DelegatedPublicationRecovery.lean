import Proofs.Session.Identity.DelegatedPublicationCases
import Proofs.Conformance.Family

namespace Conformance.DelegatedPublicationRecovery

open Lean

structure RecoveryInput where
  name : String
  deriving ToJson

structure RecoveryOutput where
  requestCommitted : Bool
  toolRunning : Bool
  childReadBeforeWatcher : Bool
  relationshipsPendingAfterWatcher : Bool
  childReadAfterWatcher : Bool
  signedByParent : Bool
  claimed : Bool
  deriving ToJson

@[conformance_family]
def watcher : Family RecoveryInput RecoveryOutput where
  inputs := [{ name := "cancelled-after-commit" }]
  model := fun _ =>
    { requestCommitted := Identity.DelegatedPublication.Cases.droppedAckState.committed
      toolRunning := Identity.DelegatedPublication.Cases.droppedAckState.toolRunning
      childReadBeforeWatcher := Identity.Household.allowed 5
        Identity.DelegatedPublication.Cases.droppedAckState.household
        "child-agent" "delivered_requests" "child-request" "read"
      relationshipsPendingAfterWatcher :=
        Identity.DelegatedPublication.Cases.watcherRecoveredState.requestPending
      childReadAfterWatcher := Identity.Household.allowed 5
        Identity.DelegatedPublication.Cases.watcherRecoveredState.household
        "child-agent" "delivered_requests" "child-request" "read"
      signedByParent := Identity.DelegatedPublication.signedBinding
        Identity.DelegatedPublication.Cases.binding
      claimed := Identity.DelegatedPublication.claim
        Identity.DelegatedPublication.Cases.watcherRecoveredState
        Identity.DelegatedPublication.Cases.grantedCommit == .claimed }

end Conformance.DelegatedPublicationRecovery
