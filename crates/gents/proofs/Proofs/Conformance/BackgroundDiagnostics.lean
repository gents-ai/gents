import Proofs.ToolCalls.Background.Diagnostics
import Proofs.Conformance.Family

namespace Conformance.BackgroundDiagnostics

open Lean BackgroundCompletion.Diagnostics

deriving instance Lean.ToJson for FailedEpochState, FailedEpoch

structure EpochOutput where
  state : FailedEpochState
  attemptCount : Nat
  pendingNotifications : Nat
  acknowledgedNotifications : Nat
  strandedNotifications : Nat
  pendingNotificationKeys : Nat
  pendingAgeSeconds : Option Nat
  oldestPendingAgeSeconds : Option Nat
  lastFailure : Option String
  nextRetryDelaySeconds : Option Nat
  deriving Lean.ToJson

@[conformance_family]
def epoch : Family FailedEpoch EpochOutput where
  inputs :=
    [ { retryCount := 0, maxRetries := 3, isLatest := true, notifications := 1,
        failureReason := "provider unavailable", ageSeconds := 6 }
    , { retryCount := 3, maxRetries := 3, isLatest := true, notifications := 1,
        failureReason := "provider unavailable", ageSeconds := 57 }
    , { retryCount := 0, maxRetries := 3, isLatest := false, notifications := 1,
        failureReason := "provider unavailable", ageSeconds := 6 }
    , { retryCount := 1, maxRetries := 3, isLatest := true, notifications := 1,
        failureReason := "provider unavailable", ageSeconds := 6 }
    , { retryCount := 2, maxRetries := 3, isLatest := true, notifications := 1,
        failureReason := "provider unavailable", ageSeconds := 6 }
    , { retryCount := 0, maxRetries := 3, isLatest := true, notifications := 0,
        failureReason := "provider unavailable", ageSeconds := 6 }
    , { retryCount := 0, maxRetries := 3, isLatest := true, notifications := 1,
        failureReason := "   ", ageSeconds := 6 }
    , { retryCount := 3, maxRetries := 3, isLatest := false, notifications := 1,
        failureReason := "provider unavailable", ageSeconds := 6 }
    ]
  model := fun e =>
    { state := failedEpochState e
      attemptCount := attemptCount e
      pendingNotifications := pendingNotifications e
      acknowledgedNotifications := acknowledgedNotifications e
      strandedNotifications := strandedNotifications e
      pendingNotificationKeys := pendingNotificationKeys e
      pendingAgeSeconds := pendingAgeSeconds e
      oldestPendingAgeSeconds := pendingAgeSeconds e
      lastFailure := lastFailure e
      nextRetryDelaySeconds := nextRetryDelaySeconds e }

end Conformance.BackgroundDiagnostics

namespace Conformance.BackgroundDiagnostics

open Lean Background.Diagnostics

deriving instance Lean.ToJson for HeadRelation, EpochState

-- `failedEpoch`'s wire structs share short names with `epoch`'s; the generated
-- Rust names come from the short name, so they live in their own namespace.
namespace HeadEpoch

structure EpochInput where
  head : HeadRelation
  retryCount : Nat
  maxRetries : Nat
  deriving Lean.ToJson

structure EpochOutput where
  state : EpochState
  nextRetryScheduled : Bool
  strandedNotifications : Nat
  deriving Lean.ToJson

end HeadEpoch

@[conformance_family]
def failedEpoch : Conformance.Family HeadEpoch.EpochInput HeadEpoch.EpochOutput where
  inputs :=
    [ { head := .differentRow, retryCount := 0, maxRetries := 3 }
    , { head := .sameRow, retryCount := 0, maxRetries := 3 }
    , { head := .absent, retryCount := 0, maxRetries := 3 }
    , { head := .differentRow, retryCount := 3, maxRetries := 3 }
    , { head := .sameRow, retryCount := 3, maxRetries := 3 }
    ]
  model := fun i =>
    { state := epochState i.retryCount i.maxRetries i.head
      nextRetryScheduled := nextRetryScheduled i.retryCount i.maxRetries i.head
      strandedNotifications := strandedNotifications i.retryCount i.maxRetries i.head }

end Conformance.BackgroundDiagnostics
