import Proofs.Store.Persistence

/-- What the session hook observes of its own mutation: in flight, acknowledged,
failed, or acknowledged as lost under its failure policy. It never reads a
committed row back. -/
inductive StorageObservation where
  | noMutation
  | inFlight
  | successAcknowledged
  | mutationFailed
  | lostAcknowledged
  deriving DecidableEq, Repr

namespace StorageObservation

def toContract : StorageObservation -> String
  | .noMutation => "noMutation"
  | .inFlight => "inFlight"
  | .successAcknowledged => "successAcknowledged"
  | .mutationFailed => "mutationFailed"
  | .lostAcknowledged => "lostAcknowledged"

instance : HasTerminal StorageObservation where
  isTerminal obs := obs = .successAcknowledged ∨ obs = .lostAcknowledged
  isTerminal_dec obs :=
    match obs with
    | .successAcknowledged => isTrue (Or.inl rfl)
    | .lostAcknowledged => isTrue (Or.inr rfl)
    | .noMutation => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))
    | .inFlight => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))
    | .mutationFailed => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))

def toPersistence : StorageObservation -> PersistenceState
  | .noMutation => .uncommitted
  | .inFlight => .committing
  | .successAcknowledged => .committed
  | .mutationFailed => .uncommitted
  | .lostAcknowledged => .lost

inductive Action where
  | beginMutation
  | mutationSuccess
  | mutationFailure
  | retryFailClosed
  | acknowledgeLost
  deriving DecidableEq, Repr

inductive Transition (policy : PersistenceState.FailurePolicy) :
    StorageObservation -> StorageObservation -> Prop where
  | begin_mutation :
      Transition policy .noMutation .inFlight
  | mutation_success :
      Transition policy .inFlight .successAcknowledged
  | mutation_failure :
      Transition policy .inFlight .mutationFailed
  | retry_fail_closed :
      policy = .failClosed ->
      Transition policy .mutationFailed .noMutation
  | acknowledge_lost :
      policy = .failOpen ->
      Transition policy .mutationFailed .lostAcknowledged

def step? (policy : PersistenceState.FailurePolicy)
    (pre : StorageObservation) : Action -> Option StorageObservation
  | .beginMutation =>
      if pre = .noMutation then some .inFlight else none
  | .mutationSuccess =>
      if pre = .inFlight then some .successAcknowledged else none
  | .mutationFailure =>
      if pre = .inFlight then some .mutationFailed else none
  | .retryFailClosed =>
      if policy = .failClosed /\ pre = .mutationFailed then some .noMutation else none
  | .acknowledgeLost =>
      if policy = .failOpen /\ pre = .mutationFailed then some .lostAcknowledged else none

inductive Trace (policy : PersistenceState.FailurePolicy) :
    StorageObservation -> StorageObservation -> Prop where
  | refl {s : StorageObservation} : Trace policy s s
  | step {s1 s2 s3 : StorageObservation} :
      Transition policy s1 s2 -> Trace policy s2 s3 -> Trace policy s1 s3

theorem begin_refines_persistence (policy : PersistenceState.FailurePolicy) :
    PersistenceState.Transition policy
      (toPersistence .noMutation) (toPersistence .inFlight) := by
  exact PersistenceState.Transition.flush

theorem success_refines_persistence (policy : PersistenceState.FailurePolicy) :
    PersistenceState.Transition policy
      (toPersistence .inFlight) (toPersistence .successAcknowledged) := by
  exact PersistenceState.Transition.write_success

theorem failure_failClosed_refines_persistence :
    PersistenceState.Transition .failClosed
      (toPersistence .inFlight) (toPersistence .mutationFailed) := by
  exact PersistenceState.Transition.write_fail_closed rfl

theorem failure_failOpen_refines_persistence :
    PersistenceState.Transition .failOpen
      (toPersistence .inFlight) (toPersistence .lostAcknowledged) := by
  exact PersistenceState.Transition.write_fail_open rfl

theorem success_acknowledged_committed :
    toPersistence .successAcknowledged = .committed := rfl

theorem mutation_failed_uncommitted :
    toPersistence .mutationFailed = .uncommitted := rfl

theorem mutation_failed_ne_committed :
    toPersistence .mutationFailed ≠ .committed := by
  decide

theorem failClosed_failed_mutation_retry_path :
    ∃ post : StorageObservation,
      Trace .failClosed .noMutation post ∧ post = .noMutation := by
  exact
    ⟨ .noMutation
    , Trace.step
        (Transition.begin_mutation (policy := .failClosed))
        (Trace.step
          (Transition.mutation_failure (policy := .failClosed))
          (Trace.step (Transition.retry_fail_closed rfl) Trace.refl))
    , rfl
    ⟩

theorem failOpen_failed_mutation_lost_path :
    ∃ post : StorageObservation,
      Trace .failOpen .noMutation post ∧ post = .lostAcknowledged := by
  exact
    ⟨ .lostAcknowledged
    , Trace.step
        (Transition.begin_mutation (policy := .failOpen))
        (Trace.step
          (Transition.mutation_failure (policy := .failOpen))
          (Trace.step (Transition.acknowledge_lost rfl) Trace.refl))
    , rfl
    ⟩

end StorageObservation
