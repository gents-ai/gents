import Proofs.Background.CompletionContinuation
import Proofs.GoalAutomation.RequestAssignment

namespace GoalAutomation.WakeReceipt

open GoalAutomation.RequestAssignment BackgroundCompletion

structure Notification where
  message : Nat
  sequence : Nat
  scope : QueueScope
  deriving DecidableEq, Repr

structure Claim where
  wake : Nat
  scope : QueueScope
  cutoff : Nat
  deriving DecidableEq, Repr

/-- Assignment chooses new capture. An exact captured notification remains
owned by that physical attempt even if its wake later opens a Goal. A message
identity and append sequence name the immutable publication revision. -/
def capture (claim : Claim) (notifications : List Notification) : WakeAttemptSnapshot :=
  ⟨claim.wake, claim.cutoff, (notifications.filter fun row =>
    row.scope == claim.scope && row.sequence ≤ claim.cutoff).map fun row =>
      ⟨row.message, row.sequence, claim.wake⟩⟩

structure Receipt where
  snapshot : WakeAttemptSnapshot
  state : RequestState
  deriving DecidableEq, Repr

def acknowledged (receipts : List Receipt) : List NotificationBinding :=
  receipts.flatMap fun receipt => receipt.snapshot.acknowledgedBindings receipt.state

/-- A retry inherits only its verified physical failed predecessor's exact
attempted publications. New publications still require the current assignment;
a Goal opened during the failed attempt cannot discard its captured input. -/
def captureWithRetry (claim : Claim) (notifications : List Notification)
    (receipts : List Receipt) (retryParent : Option Nat) : WakeAttemptSnapshot :=
  let retained := receipts.filter (fun receipt => receipt.state == .failed &&
    retryParent == some receipt.snapshot.wakeRequestId) |>.flatMap (·.snapshot.attemptedBindings)
  ⟨claim.wake, claim.cutoff, (notifications.filter fun row => row.sequence ≤ claim.cutoff &&
    (row.scope == claim.scope || retained.any fun binding =>
      binding.messageId == row.message && binding.sequence == row.sequence)).map fun row =>
      ⟨row.message, row.sequence, claim.wake⟩⟩

def current (claim : Claim) (notifications : List Notification)
    (receipts : List Receipt) (retryParent : Option Nat := none) : List NotificationBinding :=
  (captureWithRetry claim notifications receipts retryParent).attemptedBindings.filter fun binding =>
    !(acknowledged receipts).any fun prior =>
      prior.messageId == binding.messageId && prior.sequence == binding.sequence

theorem failed_receipt_acknowledges_nothing (snapshot : WakeAttemptSnapshot) :
    acknowledged [⟨snapshot, .failed⟩] = [] := by
  simp [acknowledged, WakeAttemptSnapshot.acknowledgedBindings]

theorem completed_receipt_preserves_physical_snapshot (snapshot : WakeAttemptSnapshot) :
    acknowledged [⟨snapshot, .completed⟩] = snapshot.attemptedBindings := by
  simp [acknowledged, WakeAttemptSnapshot.acknowledgedBindings]

end GoalAutomation.WakeReceipt
