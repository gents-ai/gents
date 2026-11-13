import Proofs.Store.Identity.Household

namespace Identity.PublicationConformance
open Lean
open Identity.Household

structure ParentStep where
  nativeOwner : Option String
  claimedCreator : Option String
  authenticatedCreator : Option String
  grantOk : Bool
  markOk : Bool
  revokeParent : Bool
  deriving Repr

def applyParentStep (state : ParentPublication) (step : ParentStep) : ParentPublication :=
  let repaired := recoverParent state step.nativeOwner step.claimedCreator
    step.authenticatedCreator step.grantOk step.markOk
  if step.revokeParent then ownerRevokesLinkedParent repaired else repaired

def parentStateJson (state : ParentPublication) : Json := Json.mkObj
  [("committed", toJson state.committed), ("parent_granted", toJson state.parentGranted),
   ("parent_linked", toJson state.parentLinked)]

def parentStepJson (state : ParentPublication) (step : ParentStep) : Json := Json.mkObj
  [("native_owner", toJson step.nativeOwner), ("claimed_creator", toJson step.claimedCreator),
   ("authenticated_creator", toJson step.authenticatedCreator), ("grant_ok", toJson step.grantOk),
   ("mark_ok", toJson step.markOk), ("revoke_parent", toJson step.revokeParent),
   ("after", parentStateJson (applyParentStep state step))]

def parentTraceJson : ParentPublication → List ParentStep → List Json
  | _, [] => []
  | state, step :: rest => parentStepJson state step :: parentTraceJson (applyParentStep state step) rest

def parentCases : List (String × List ParentStep) :=
  [("grant_failure_then_recovery", [⟨some "agent", some "agent", some "agent", false, true, false⟩,
      ⟨some "agent", some "agent", some "agent", true, true, false⟩]),
   ("marker_failure_then_recovery", [⟨some "agent", some "agent", some "agent", true, false, false⟩,
      ⟨some "agent", some "agent", some "agent", false, true, false⟩]),
   ("forged_creator", [⟨some "agent", some "other", some "agent", true, true, false⟩]),
   ("missing_authenticated_creator", [⟨some "agent", some "agent", none, true, true, false⟩]),
   ("revoked_after_completion", [⟨some "agent", some "agent", some "agent", true, true, false⟩,
      ⟨some "agent", some "agent", some "agent", true, true, true⟩])]

def parentCasesJson : String := (toJson (parentCases.map fun (name, steps) => Json.mkObj
  [("name", toJson name), ("initial", parentStateJson beginParentPublication),
   ("steps", toJson (parentTraceJson beginParentPublication steps))])).compress

end Identity.PublicationConformance
