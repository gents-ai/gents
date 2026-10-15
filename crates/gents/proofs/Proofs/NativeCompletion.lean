/-!
# Native storage completion ownership

This model states obligations for Gents' owner tasks around DefraDB operations.
`nativeReturned` is an observation supplied by the adapter; a caller timeout or
cancellation does not imply it. The model does not establish that native work
eventually returns. If it never returns, retaining the mutation gate is the
safe modeled outcome. Explicit rollback completion is allowed only after all
abandoned native statements return. Native owner abandonment instead removes the
registry entry synchronously, without implying that already-acquired contexts
have returned. Both paths retain the gate until every native operation drains.
This model observes registry ownership and gate ownership, not storage internals.
-/
namespace NativeCompletion

inductive Caller where
  | waiting | returned | timedOut | cancelled
  deriving DecidableEq, Repr

structure State where
  caller : Caller
  nativeOutstanding : Nat
  poisoned : Bool
  cleanupRequired : Bool
  registryAbandoned : Bool
  rollbackReturned : Bool
  gateHeld : Bool
  retryStarted : Bool
  periodicRenewal : Bool
  renewalAttemptReturned : Bool
  nextRenewalStarted : Bool
  deriving DecidableEq, Repr

inductive Event where
  | nativeStarted | nativeReturned | callerReturned | callerTimedOut | callerCancelled
  | registryAbandoned | rollbackReturned | retryRequested | gateReleaseRequested
  | renewalAttemptReturned | renewalRequested
  deriving DecidableEq, Repr

def nativeAdmissionAllowed (poisoned : Bool) : Bool := !poisoned

def nativeWorkDrained (nativeOutstanding : Nat) : Bool := nativeOutstanding == 0

def attemptFinalizationAllowed (poisoned : Bool) (nativeOutstanding : Nat)
    (cleanupRequired rollbackReturned : Bool) : Bool :=
  nativeAdmissionAllowed poisoned && nativeWorkDrained nativeOutstanding &&
    (!cleanupRequired || rollbackReturned)

def step (s : State) (event : Event) : State :=
  match event with
  | .nativeStarted =>
      if !nativeAdmissionAllowed s.poisoned || (!s.gateHeld && !s.periodicRenewal) then s
      else { s with nativeOutstanding := s.nativeOutstanding + 1 }
  | .nativeReturned =>
      { s with nativeOutstanding := s.nativeOutstanding - 1 }
  | .callerReturned => { s with caller := .returned }
  | .callerTimedOut => { s with caller := .timedOut, poisoned := true }
  | .callerCancelled => { s with caller := .cancelled, poisoned := true }
  | .registryAbandoned =>
      if s.cleanupRequired then { s with registryAbandoned := true, poisoned := true }
      else s
  | .rollbackReturned =>
      if nativeWorkDrained s.nativeOutstanding && s.cleanupRequired then
        { s with rollbackReturned := true }
      else s
  | .retryRequested =>
      if attemptFinalizationAllowed s.poisoned s.nativeOutstanding
          s.cleanupRequired s.rollbackReturned then
        { s with retryStarted := true }
      else s
  | .gateReleaseRequested =>
      if nativeWorkDrained s.nativeOutstanding &&
          (!s.cleanupRequired || s.rollbackReturned || s.registryAbandoned) then
        { s with gateHeld := false }
      else s
  | .renewalAttemptReturned =>
      if s.periodicRenewal && nativeWorkDrained s.nativeOutstanding then
        { s with renewalAttemptReturned := true }
      else s
  | .renewalRequested =>
      if s.periodicRenewal && s.renewalAttemptReturned &&
          nativeWorkDrained s.nativeOutstanding then
        { s with nextRenewalStarted := true }
      else s

def run (s : State) (events : List Event) : State := events.foldl step s

def transaction : State := ⟨.waiting, 0, false, true, false, false, true, false, false, false, false⟩
def autoCommit : State := ⟨.waiting, 0, false, false, false, false, true, false, false, false, false⟩

theorem timeout_is_not_native_completion :
    (step (step transaction .nativeStarted) .callerTimedOut).nativeOutstanding = 1 ∧
      (step (step transaction .nativeStarted) .callerTimedOut).caller = .timedOut := by
  decide

theorem cancelled_operation_poisoned_without_replay :
    let s := run transaction [.nativeStarted, .callerCancelled, .retryRequested]
    s.nativeOutstanding = 1 ∧ s.poisoned = true ∧ s.retryStarted = false := by
  decide

theorem timeout_blocks_cleanup_and_release_while_native_live :
    let s := run transaction
      [.nativeStarted, .callerTimedOut, .rollbackReturned, .gateReleaseRequested]
    s.nativeOutstanding = 1 ∧ s.rollbackReturned = false ∧ s.gateHeld = true := by
  decide

theorem transaction_cleanup_follows_observed_native_and_rollback_return :
    let s := run transaction
      [.nativeStarted, .callerTimedOut, .nativeReturned, .rollbackReturned,
        .gateReleaseRequested]
    s.nativeOutstanding = 0 ∧ s.rollbackReturned = true ∧ s.gateHeld = false := by
  decide

theorem auto_commit_gate_tracks_native_completion :
    let before := run autoCommit [.nativeStarted, .callerTimedOut, .gateReleaseRequested]
    let after := run before [.nativeReturned, .gateReleaseRequested]
    before.gateHeld = true ∧ after.gateHeld = false := by
  decide

theorem abandoned_transaction_never_retries :
    (run transaction
      [.nativeStarted, .callerTimedOut, .nativeReturned, .rollbackReturned,
        .retryRequested]).retryStarted = false := by
  decide

theorem any_outstanding_native_work_blocks_gate_release (s : State)
    (h : 0 < s.nativeOutstanding) (gate : s.gateHeld = true) :
    (step s .gateReleaseRequested).gateHeld = true := by
  simp [step, nativeWorkDrained, Nat.ne_of_gt h, gate]

theorem poisoned_transaction_never_retries (s : State) (h : s.poisoned = true) :
    (step s .retryRequested).retryStarted = s.retryStarted := by
  simp [step, attemptFinalizationAllowed, nativeAdmissionAllowed, h]

theorem concurrent_operations_are_all_drained_before_rollback :
    let s := run transaction
      [.nativeStarted, .nativeStarted, .callerTimedOut, .nativeReturned,
        .rollbackReturned]
    s.nativeOutstanding = 1 ∧ s.rollbackReturned = false ∧ s.gateHeld = true := by
  decide

/--
Execution-lease renewal intentionally bypasses the ordinary mutation gate so
lease maintenance can progress during other writes. Its single `RenewalTask`
owner must nevertheless wait for an abandoned native statement before it
returns to the polling loop and admits another renewal attempt.
-/
theorem periodic_renewal_waits_for_abandoned_native_work :
    let renewal := { transaction with periodicRenewal := true, gateHeld := false }
    let s := run renewal
      [.nativeStarted, .callerTimedOut, .renewalAttemptReturned, .renewalRequested]
    s.nativeOutstanding = 1 ∧ s.renewalAttemptReturned = false ∧
      s.nextRenewalStarted = false := by
  decide

theorem periodic_renewal_resumes_after_native_return :
    let renewal := { transaction with periodicRenewal := true, gateHeld := false }
    let s := run renewal
      [.nativeStarted, .callerTimedOut, .nativeReturned, .renewalAttemptReturned,
        .renewalRequested]
    s.nativeOutstanding = 0 ∧ s.renewalAttemptReturned = true ∧
      s.nextRenewalStarted = true := by
  decide

theorem registry_abandonment_does_not_imply_drain_or_gate_release :
    let s := run transaction
      [.nativeStarted, .nativeStarted, .callerTimedOut, .registryAbandoned,
        .nativeReturned, .gateReleaseRequested, .retryRequested]
    s.registryAbandoned = true ∧ s.nativeOutstanding = 1 ∧
      s.rollbackReturned = false ∧ s.gateHeld = true ∧ s.retryStarted = false := by
  decide

theorem abandoned_registry_releases_gate_only_after_full_drain :
    let s := run transaction
      [.nativeStarted, .nativeStarted, .callerTimedOut, .registryAbandoned,
        .nativeReturned, .gateReleaseRequested, .nativeReturned, .gateReleaseRequested]
    s.registryAbandoned = true ∧ s.nativeOutstanding = 0 ∧
      s.rollbackReturned = false ∧ s.gateHeld = false := by
  decide

end NativeCompletion
