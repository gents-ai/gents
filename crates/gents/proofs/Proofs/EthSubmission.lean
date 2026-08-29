import Proofs.Basic

namespace EthSubmission

/-- Durable states for one idempotent Ethereum submission. -/
inductive Status where
  | prepared
  | submittedUnknown
  | confirmedSuccess
  | confirmedReverted
  deriving DecidableEq, Repr

def Status.all : List Status :=
  [.prepared, .submittedUnknown, .confirmedSuccess, .confirmedReverted]

def Status.toDefraDB : Status → String
  | .prepared => "prepared"
  | .submittedUnknown => "submitted_unknown"
  | .confirmedSuccess => "confirmed_success"
  | .confirmedReverted => "confirmed_reverted"

instance : HasTerminal Status where
  isTerminal status :=
    status = .confirmedSuccess ∨ status = .confirmedReverted
  isTerminal_dec _ := inferInstance

inductive Action where
  | broadcast
  | observeSuccess
  | observeRevert
  deriving DecidableEq, Repr

/-- Broadcast is retryable only by reusing the already prepared bytes. A
receipt is authoritative even if a crash left the journal at `prepared`. -/
def Status.step? : Status → Action → Option Status
  | .prepared, .broadcast => some .submittedUnknown
  | .submittedUnknown, .broadcast => some .submittedUnknown
  | .prepared, .observeSuccess => some .confirmedSuccess
  | .submittedUnknown, .observeSuccess => some .confirmedSuccess
  | .prepared, .observeRevert => some .confirmedReverted
  | .submittedUnknown, .observeRevert => some .confirmedReverted
  | _, _ => none

structure Submission where
  status : Status
  submissionKey : Nat
  requestHash : Nat
  rawTransactionHash : Nat
  deriving DecidableEq, Repr

def Submission.step? (submission : Submission) (action : Action) : Option Submission :=
  (Status.step? submission.status action).map fun status =>
    { submission with status := status }

/-- Every legal transition retains the idempotency identity and exact signed
transaction. Recovery can poll or rebroadcast, but cannot manufacture bytes. -/
theorem step_preserves_prepared_identity
    (before after : Submission) (action : Action)
    (h : before.step? action = some after) :
    after.submissionKey = before.submissionKey ∧
      after.requestHash = before.requestHash ∧
      after.rawTransactionHash = before.rawTransactionHash := by
  unfold Submission.step? at h
  cases hStep : Status.step? before.status action <;> simp [hStep] at h
  subst after
  simp

theorem confirmed_success_is_absorbing (action : Action) :
    Status.step? .confirmedSuccess action = none := by
  cases action <;> rfl

theorem confirmed_reverted_is_absorbing (action : Action) :
    Status.step? .confirmedReverted action = none := by
  cases action <;> rfl

theorem rebroadcast_reuses_signed_transaction (submission : Submission)
    (h : submission.status = .submittedUnknown) :
    (submission.step? .broadcast).map Submission.rawTransactionHash =
      some submission.rawTransactionHash := by
  rcases submission with ⟨status, submissionKey, requestHash, rawTransactionHash⟩
  cases status <;> simp_all [Submission.step?, Status.step?]

end EthSubmission
