import Proofs.ToolCalls.Background.CompletionContinuation

/-!
# Background Completion Diagnostics

The diagnostics projection classifies a background-completion epoch by its
latest wake. This module models the failed-wake branch: the epoch state, the
notifications it strands, the next redrive instant and the failure the
operator is shown. The production owner is `summarize` in
`crates/gents-toolcalls/src/background/diagnostics.rs`.
-/

namespace BackgroundCompletion.Diagnostics

/-- Classification of an epoch whose latest wake failed. An exhausted budget
wins over the head check; a wake that is no longer the session head is not
retried; otherwise the epoch is in backoff. -/
inductive FailedEpochState where
  | exhausted
  | retryIneligibleNotLatest
  | retryBackoff
  deriving DecidableEq, Repr

/-- A failed latest wake and the scenario it is observed in. `ageSeconds` is the
time from the first attempt's creation to the diagnostics instant. -/
structure FailedEpoch where
  retryCount : Nat
  maxRetries : Nat
  isLatest : Bool
  notifications : Nat
  failureReason : String
  ageSeconds : Nat

def failedEpochState (epoch : FailedEpoch) : FailedEpochState :=
  if epoch.maxRetries ≤ epoch.retryCount then .exhausted
  else if epoch.isLatest then .retryBackoff
  else .retryIneligibleNotLatest

/-- A failed attempt acknowledges no notification, so every bound notification
is pending. -/
def acknowledgedNotifications (_epoch : FailedEpoch) : Nat := 0

def pendingNotifications (epoch : FailedEpoch) : Nat :=
  epoch.notifications - acknowledgedNotifications epoch

def pendingNotificationKeys (epoch : FailedEpoch) : Nat :=
  pendingNotifications epoch

/-- Notifications are stranded when the epoch can no longer redrive them. -/
def strandedNotifications (epoch : FailedEpoch) : Nat :=
  match failedEpochState epoch with
  | .retryBackoff => 0
  | .exhausted | .retryIneligibleNotLatest => pendingNotifications epoch

def attemptCount (epoch : FailedEpoch) : Nat := epoch.retryCount + 1

/-- The redrive is due only for the head wake with budget left, after the
shared retry backoff. -/
def nextRetryDelaySeconds (epoch : FailedEpoch) : Option Nat :=
  if epoch.isLatest && decide (epoch.retryCount < epoch.maxRetries) then
    some (wakeRetryDelaySeconds epoch.retryCount)
  else none

/-- Nothing acknowledges a failed wake, so its pending age is always reported. -/
def pendingAgeSeconds (epoch : FailedEpoch) : Option Nat := some epoch.ageSeconds

def lastFailure (epoch : FailedEpoch) : Option String :=
  if epoch.failureReason.trim.isEmpty then none else some epoch.failureReason

def backoffEpoch : FailedEpoch :=
  { retryCount := 0, maxRetries := 3, isLatest := true, notifications := 1,
    failureReason := "provider unavailable", ageSeconds := 6 }

def exhaustedEpoch : FailedEpoch :=
  { retryCount := 3, maxRetries := 3, isLatest := true, notifications := 1,
    failureReason := "provider unavailable", ageSeconds := 57 }

def displacedEpoch : FailedEpoch :=
  { retryCount := 0, maxRetries := 3, isLatest := false, notifications := 1,
    failureReason := "provider unavailable", ageSeconds := 6 }

theorem failed_epoch_surfaces_backoff_and_unacknowledged_notification :
    failedEpochState backoffEpoch = .retryBackoff ∧
      pendingNotifications backoffEpoch = 1 ∧
      acknowledgedNotifications backoffEpoch = 0 ∧
      strandedNotifications backoffEpoch = 0 ∧
      attemptCount backoffEpoch = 1 ∧
      pendingAgeSeconds backoffEpoch = some 6 ∧
      nextRetryDelaySeconds backoffEpoch = some 5 := by
  decide

theorem exhausted_epoch_is_stranded :
    failedEpochState exhaustedEpoch = .exhausted ∧
      strandedNotifications exhaustedEpoch = 1 ∧
      nextRetryDelaySeconds exhaustedEpoch = none := by
  decide

theorem failed_wake_displaced_by_later_turn_is_stranded_not_retrying :
    failedEpochState displacedEpoch = .retryIneligibleNotLatest ∧
      strandedNotifications displacedEpoch = 1 ∧
      pendingNotificationKeys displacedEpoch = 1 ∧
      nextRetryDelaySeconds displacedEpoch = none := by
  decide

end BackgroundCompletion.Diagnostics

/-!
## Physical request head

`summarize` in gents-toolcalls classifies a background-completion epoch for
operators. This module models one epoch: a single failed scheduled wake whose
completion notification is still unacknowledged, with no successor request.

The session head is the request document that the session's latest-request
read names. A wake is retry-eligible only while the head is that exact physical
document. A different physical row that shares the wake's logical request id
is not the head, so the wake is not retried.
-/

namespace Background.Diagnostics

/-- How the session head relates to the failed wake's physical row. A
`differentRow` head may carry the same logical request id as the wake. -/
inductive HeadRelation where
  | sameRow
  | differentRow
  | absent
  deriving DecidableEq, Repr

inductive EpochState where
  | exhausted
  | retryIneligibleNotLatest
  | retryBackoff
  deriving DecidableEq, Repr

def retryIsLatest : HeadRelation → Bool
  | .sameRow => true
  | .differentRow => false
  | .absent => false

def epochState (retryCount maxRetries : Nat) (head : HeadRelation) : EpochState :=
  if maxRetries ≤ retryCount then .exhausted
  else if retryIsLatest head then .retryBackoff
  else .retryIneligibleNotLatest

def nextRetryScheduled (retryCount maxRetries : Nat) (head : HeadRelation) : Bool :=
  epochState retryCount maxRetries head == .retryBackoff

/-- The epoch has one unacknowledged notification. It is stranded unless a retry
is still backing off. -/
def strandedNotifications (retryCount maxRetries : Nat) (head : HeadRelation) : Nat :=
  if epochState retryCount maxRetries head == .retryBackoff then 0 else 1

theorem retry_eligibility_requires_the_exact_physical_request_head :
    epochState 0 3 .differentRow = .retryIneligibleNotLatest ∧
      nextRetryScheduled 0 3 .differentRow = false ∧
      strandedNotifications 0 3 .differentRow = 1 := by
  decide

theorem exact_physical_head_keeps_the_retry :
    epochState 0 3 .sameRow = .retryBackoff ∧
      nextRetryScheduled 0 3 .sameRow = true ∧
      strandedNotifications 0 3 .sameRow = 0 := by
  decide

theorem absent_head_is_not_retry_eligible :
    epochState 0 3 .absent = .retryIneligibleNotLatest ∧
      nextRetryScheduled 0 3 .absent = false := by
  decide

theorem exhausted_epoch_is_stranded_whatever_the_head :
    epochState 3 3 .sameRow = .exhausted ∧
      epochState 3 3 .differentRow = .exhausted ∧
      strandedNotifications 3 3 .sameRow = 1 ∧
      strandedNotifications 3 3 .differentRow = 1 := by
  decide

end Background.Diagnostics
