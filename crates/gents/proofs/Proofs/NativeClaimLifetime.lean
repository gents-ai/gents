import Proofs.NativeCompletion

/-!
# Claimed local-store lifetime

A local store claim is attached to the per-node gate resource, independently
of the mutation mutex. Access owners retain that resource while live; admitted
native work and detached cleanup retain it until they actually return. Read-only
transactions, renewals, claimed reads, and schema registration retain the
resource without holding the mutation mutex. `nativeOutstanding` counts
Gents-owned native dispatch/future lifetime, including a detached dispatch or
an async schema call whose completion owner remains live; timeout/cancellation
does not establish native completion. This model does not claim eventual
native completion, OS lock behavior, or shutdown completion.
-/
namespace NativeClaimLifetime

inductive Lane where
  | mutationTransaction
  | autoCommitWrite
  | readOnlyTransaction
  | periodicRenewal
  | claimedRead
  | claimedSchema
  deriving DecidableEq, Repr

inductive Caller where
  | waiting | returned | timedOut | cancelled
  deriving DecidableEq, Repr

structure State where
  lane : Lane
  caller : Caller
  claimAttached : Bool
  accessOwnerAlive : Bool
  transactionOwnerAlive : Bool
  nativeOutstanding : Nat
  poisoned : Bool
  cleanupPending : Bool
  rollbackReturned : Bool
  mutationGateHeld : Bool
  retryStarted : Bool
  renewalAttemptReturned : Bool
  nextRenewalStarted : Bool
  deriving DecidableEq, Repr

def claimHeld (s : State) : Bool :=
  s.claimAttached &&
    (s.accessOwnerAlive || s.transactionOwnerAlive ||
      0 < s.nativeOutstanding || s.cleanupPending || s.mutationGateHeld)

inductive Event where
  | nativeStarted | nativeReturned | callerReturned | callerTimedOut | callerCancelled
  | accessReleased | transactionDropped | transactionCommitted | rollbackReturned
  | retryRequested | gateReleaseRequested | renewalAttemptReturned | renewalRequested
  deriving DecidableEq, Repr

def isTransaction (lane : Lane) : Bool :=
  match lane with
  | .mutationTransaction | .readOnlyTransaction | .periodicRenewal => true
  | .autoCommitWrite | .claimedRead | .claimedSchema => false

def step (s : State) (event : Event) : State :=
  match event with
  | .nativeStarted =>
      if NativeCompletion.nativeAdmissionAllowed s.poisoned &&
          (if isTransaction s.lane then s.transactionOwnerAlive else s.accessOwnerAlive) &&
          (s.lane != .mutationTransaction || s.mutationGateHeld) then
        { s with nativeOutstanding := s.nativeOutstanding + 1 }
      else s
  | .nativeReturned =>
      if s.nativeOutstanding == 0 then s
      else { s with nativeOutstanding := s.nativeOutstanding - 1 }
  | .callerReturned => { s with caller := .returned }
  | .callerTimedOut => { s with caller := .timedOut, poisoned := true }
  | .callerCancelled => { s with caller := .cancelled, poisoned := true }
  | .accessReleased => { s with accessOwnerAlive := false }
  | .transactionDropped =>
      if isTransaction s.lane && s.transactionOwnerAlive then
        { s with transactionOwnerAlive := false, cleanupPending := true }
      else s
  | .transactionCommitted =>
      if isTransaction s.lane && s.transactionOwnerAlive && !s.cleanupPending &&
          NativeCompletion.attemptFinalizationAllowed s.poisoned s.nativeOutstanding
            false s.rollbackReturned then
        { s with transactionOwnerAlive := false, cleanupPending := false }
      else s
  | .rollbackReturned =>
      if isTransaction s.lane && s.nativeOutstanding == 0 && s.cleanupPending then
        { s with cleanupPending := false, rollbackReturned := true }
      else s
  | .retryRequested =>
      if NativeCompletion.attemptFinalizationAllowed s.poisoned s.nativeOutstanding
          s.cleanupPending s.rollbackReturned then
        { s with retryStarted := true }
      else s
  | .gateReleaseRequested =>
      if s.nativeOutstanding == 0 && !s.cleanupPending then
        { s with mutationGateHeld := false }
      else s
  | .renewalAttemptReturned =>
      if s.lane == .periodicRenewal && s.nativeOutstanding == 0 then
        { s with renewalAttemptReturned := true }
      else s
  | .renewalRequested =>
      if s.lane == .periodicRenewal && s.renewalAttemptReturned &&
          s.nativeOutstanding == 0 then
        { s with nextRenewalStarted := true }
      else s

def run (s : State) (events : List Event) : State := events.foldl step s

def mutationTransaction : State :=
  ⟨.mutationTransaction, .waiting, true, true, true, 0, false, false, false,
    true, false, false, false⟩

def autoCommitWrite : State :=
  ⟨.autoCommitWrite, .waiting, true, true, false, 0, false, false, false,
    true, false, false, false⟩

def readOnlyTransaction : State :=
  ⟨.readOnlyTransaction, .waiting, true, true, true, 0, false, false, false,
    false, false, false, false⟩

def periodicRenewal : State :=
  ⟨.periodicRenewal, .waiting, true, true, true, 0, false, false, false,
    false, false, false, false⟩

def claimedRead : State :=
  ⟨.claimedRead, .waiting, true, true, false, 0, false, false, false,
    false, false, false, false⟩

def claimedSchema : State :=
  ⟨.claimedSchema, .waiting, true, true, false, 0, false, false, false,
    false, false, false, false⟩

theorem timeout_does_not_release_claim_while_native_live :
    let s := run claimedRead
      [.nativeStarted, .callerTimedOut, .accessReleased, .gateReleaseRequested]
    s.caller = .timedOut ∧ s.nativeOutstanding = 1 ∧ claimHeld s = true := by
  decide

theorem cancellation_does_not_release_claim_while_native_live :
    let s := run autoCommitWrite
      [.nativeStarted, .callerCancelled, .accessReleased, .gateReleaseRequested]
    s.caller = .cancelled ∧ s.nativeOutstanding = 1 ∧ claimHeld s = true := by
  decide

theorem claimed_read_needs_no_mutation_gate :
    let s := run claimedRead [.nativeStarted, .accessReleased]
    s.mutationGateHeld = false ∧ s.nativeOutstanding = 1 ∧ claimHeld s = true := by
  decide

theorem schema_future_retains_claim_until_actual_return :
    let before := run claimedSchema
      [.nativeStarted, .callerTimedOut, .accessReleased, .gateReleaseRequested]
    let after := run before [.nativeReturned, .gateReleaseRequested]
    before.mutationGateHeld = false ∧ before.nativeOutstanding = 1 ∧
      claimHeld before = true ∧ claimHeld after = false := by
  decide

theorem transaction_drop_transfers_claim_to_pending_cleanup :
    let s := run readOnlyTransaction
      [.nativeStarted, .callerCancelled, .transactionDropped, .accessReleased]
    s.transactionOwnerAlive = false ∧ s.nativeOutstanding = 1 ∧
      s.cleanupPending = true ∧ claimHeld s = true := by
  decide

theorem rollback_cannot_release_claim_before_native_return :
    let s := run readOnlyTransaction
      [.nativeStarted, .callerTimedOut, .transactionDropped,
        .accessReleased, .rollbackReturned, .gateReleaseRequested]
    s.nativeOutstanding = 1 ∧ s.rollbackReturned = false ∧
      s.mutationGateHeld = false ∧ claimHeld s = true := by
  decide

theorem mutation_transaction_claim_survives_timeout_cleanup :
    let before := run mutationTransaction
      [.nativeStarted, .callerTimedOut, .transactionDropped,
        .accessReleased, .rollbackReturned, .gateReleaseRequested]
    let after := run before
      [.nativeReturned, .rollbackReturned, .gateReleaseRequested]
    claimHeld before = true ∧ before.cleanupPending = true ∧
      before.mutationGateHeld = true ∧ claimHeld after = false := by
  decide

theorem auto_commit_claim_follows_native_completion :
    let before := run autoCommitWrite
      [.nativeStarted, .callerCancelled, .accessReleased, .gateReleaseRequested]
    let after := run before [.nativeReturned, .gateReleaseRequested]
    claimHeld before = true ∧ claimHeld after = false := by
  decide

theorem committed_transaction_leaves_claim_with_live_access :
    let before := run readOnlyTransaction
      [.nativeStarted, .nativeReturned, .transactionCommitted,
        .gateReleaseRequested]
    let after := run before [.accessReleased]
    before.transactionOwnerAlive = false ∧ before.mutationGateHeld = false ∧
      before.accessOwnerAlive = true ∧ claimHeld before = true ∧
      claimHeld after = false := by
  decide

theorem readonly_and_renewal_retain_claim_without_mutex :
    let read := run readOnlyTransaction
      [.nativeStarted, .callerCancelled, .transactionDropped, .accessReleased]
    let renewal := run periodicRenewal
      [.nativeStarted, .callerTimedOut, .renewalAttemptReturned, .renewalRequested]
    read.mutationGateHeld = false ∧ read.nativeOutstanding = 1 ∧
      read.cleanupPending = true ∧ claimHeld read = true ∧
      renewal.mutationGateHeld = false ∧ renewal.nativeOutstanding = 1 ∧
      renewal.renewalAttemptReturned = false ∧ renewal.nextRenewalStarted = false ∧
      claimHeld renewal = true := by
  decide

theorem renewal_advances_only_after_observed_native_return :
    let before := run periodicRenewal
      [.nativeStarted, .callerTimedOut, .renewalAttemptReturned, .renewalRequested]
    let after := run before
      [.nativeReturned, .renewalAttemptReturned, .renewalRequested]
    before.nextRenewalStarted = false ∧ after.nextRenewalStarted = true ∧
      claimHeld after = true := by
  decide

theorem claim_releases_after_access_and_cleanup_end :
    let read := run claimedRead
      [.nativeStarted, .accessReleased, .nativeReturned]
    let txn := run readOnlyTransaction
      [.nativeStarted, .transactionDropped, .accessReleased, .nativeReturned,
        .rollbackReturned, .gateReleaseRequested]
  claimHeld read = false ∧ claimHeld txn = false := by
  decide

theorem drained_poisoned_transaction_cannot_restart_or_commit :
    let s := run mutationTransaction
      [.nativeStarted, .callerTimedOut, .nativeReturned]
    let restarted := step s .nativeStarted
    let committed := step s .transactionCommitted
    s.poisoned = true ∧ s.nativeOutstanding = 0 ∧ s.transactionOwnerAlive = true ∧
      restarted.nativeOutstanding = 0 ∧ committed.transactionOwnerAlive = true ∧
      committed.cleanupPending = false := by
  decide

end NativeClaimLifetime
