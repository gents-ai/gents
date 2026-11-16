import Proofs.Session.Request.CrossActorAuthorization
import Proofs.Conformance.Family

namespace Conformance.PendingRejection

open Lean Request.CrossActorAuthorization

deriving instance Lean.ToJson for PendingRejectionOutcome, PendingRejectionSelection

structure RejectionOutput where
  outcome : PendingRejectionOutcome
  deriving ToJson

@[conformance_family]
def selection : Conformance.Family PendingRejectionSelection RejectionOutput where
  inputs :=
    [false, true].flatMap fun nodeIsLocal =>
      [false, true].flatMap fun agentIsNode =>
        [false, true].map fun identityIsAgent => { nodeIsLocal, agentIsNode, identityIsAgent }
  model := fun s => { outcome := pendingRejectionOutcome s }

end Conformance.PendingRejection
