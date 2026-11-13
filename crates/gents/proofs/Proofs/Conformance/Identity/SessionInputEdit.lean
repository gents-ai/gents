import Proofs.Session.Identity.SessionInputEdit
import Proofs.Conformance.Family

namespace Conformance.SessionInputEdit

open Lean Identity.SessionInputEdit

structure ClaimInput where
  name : String
  selectedAllowed : Bool
  deriving ToJson

structure ClaimOutput where
  tamperedAccepted : Bool
  applied : Bool
  rollbackReplacement : Bool
  rollbackReceipt : Bool
  noRegrant : Bool
  unprovenRemovalPending : Bool
  deriving ToJson

private def claimInputs : List ClaimInput :=
  [⟨"allowed", true⟩, ⟨"selected-denied", false⟩]

private def rolledBack : Request.ClientSignedEditHandoff.Effect :=
  Request.ClientSignedEditHandoff.stage Request.CrossActorAuthorization.exampleIntent
    ⟨"user", "session-doc", "agent-doc", "input", "evidence", true⟩
    "input" "evidence" ⟨true, true, false⟩

/-- The rollback, revocation and lost-receipt observations do not depend on
which request the command selects: the served scenario makes each of them for
every input. -/
@[conformance_family]
def claim : Family ClaimInput ClaimOutput where
  inputs := claimInputs
  model := fun i =>
    { tamperedAccepted := (Request.ClientSignedEditHandoff.stage
        Request.CrossActorAuthorization.exampleIntent
        ⟨"user", "session-doc", "agent-doc", "input", "evidence", true⟩
        "input" "changed" ⟨true, true, true⟩).replacement
      applied := apply .pending Request.CrossActorAuthorization.exampleIntent
        Request.CrossActorAuthorization.permittedObservation ⟨true, true, true, true⟩
        (some [i.selectedAllowed]) (some [i.selectedAllowed]) == Outcome.applied
      rollbackReplacement := rolledBack.replacement
      rollbackReceipt := rolledBack.receipt
      noRegrant := Request.ClientSignedEditHandoff.recover true ⟨true, true, true⟩ ==
        (⟨false, false, false⟩ : Request.ClientSignedEditHandoff.Effect)
      unprovenRemovalPending := unprovenRemovalReplay false true }

structure DiscoveryInput where
  actor : String
  deriving ToJson

structure DiscoveryOutput where
  visible : Bool
  selected : Bool
  deriving ToJson

@[conformance_family]
def nativeDiscovery : Family DiscoveryInput DiscoveryOutput where
  inputs := ["agent", "other"].map fun actor => { actor }
  model := fun i =>
    { visible := !(discover fixture [i.actor] "edit-doc").isEmpty
      selected := !(selectedForEffect (discover fixture [i.actor] "edit-doc") "agent").isEmpty }

end Conformance.SessionInputEdit
