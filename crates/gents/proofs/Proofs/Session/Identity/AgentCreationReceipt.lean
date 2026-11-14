import Proofs.Session.DurableLineage

/-! Whether a session's creation receipt authorizes an Agent-started session
for the opening request its caller names. -/

namespace Identity.AgentCreationReceipt

inductive Cause where
  | agentNew
  | taskTrigger
  | pluginJob
  deriving DecidableEq, Repr

/-- Only a receipt for a session an Agent started (`agent_new` or a plugin
job) is creation authority for that Agent; a task-trigger receipt is signed by
the selected Agent itself and authorizes nothing here. -/
def creatorCause : Cause → Bool
  | .agentNew => true
  | .pluginJob => true
  | .taskTrigger => false

/-- A caller that names an opening request accepts only a receipt signed for
that exact opening; a caller that names none accepts the signed opening. -/
def openingAccepted (expected : Option String) (signedOpening : String) : Bool :=
  match expected with
  | none => true
  | some opening => opening == signedOpening

def verifies (signatureValid scopeMatches : Bool) (cause : Cause)
    (expected : Option String) (signedOpening : String) : Bool :=
  DurableLineage.verifiedCreation signatureValid scopeMatches
    (openingAccepted expected signedOpening) (creatorCause cause)

theorem a_receipt_never_verifies_for_another_opening
    (signatureValid scopeMatches : Bool) (cause : Cause) (expected signedOpening : String)
    (h : expected ≠ signedOpening) :
    verifies signatureValid scopeMatches cause (some expected) signedOpening = false := by
  simp [verifies, openingAccepted, DurableLineage.verifiedCreation, h]

theorem a_task_trigger_receipt_never_verifies
    (signatureValid scopeMatches : Bool) (expected : Option String) (signedOpening : String) :
    verifies signatureValid scopeMatches .taskTrigger expected signedOpening = false := by
  simp [verifies, creatorCause, DurableLineage.verifiedCreation]

theorem an_unsigned_receipt_never_verifies
    (scopeMatches : Bool) (cause : Cause) (expected : Option String) (signedOpening : String) :
    verifies false scopeMatches cause expected signedOpening = false := by
  simp [verifies, DurableLineage.verifiedCreation]

end Identity.AgentCreationReceipt
