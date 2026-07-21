import Proofs.Persistence

/-!
# Storage Observation Boundary

This module models the daemon-visible storage facts used by persistence and
session code. It deliberately does not model DefraDB internals. Instead, it
records the minimum observations the daemon is allowed to rely on:

* an awaited successful mutation is treated as a committed write,
* a failed mutation is not treated as committed,
* stale reads or missing events do not regress a successful mutation ack, and
* a successful ack has read-your-writes and eventual-observation paths.
-/

/-- Daemon-local observations at the storage boundary. These are contract names,
not persisted DefraDB values. -/
inductive StorageObservation where
  | noMutation
  | inFlight
  | successAcknowledged
  | mutationFailed
  | staleObserved
  | readVisible
  | lostAcknowledged
  deriving DecidableEq, Repr

namespace StorageObservation

/-- Stable contract vocabulary for Rust tests. -/
def toContract : StorageObservation -> String
  | .noMutation => "noMutation"
  | .inFlight => "inFlight"
  | .successAcknowledged => "successAcknowledged"
  | .mutationFailed => "mutationFailed"
  | .staleObserved => "staleObserved"
  | .readVisible => "readVisible"
  | .lostAcknowledged => "lostAcknowledged"

/-- Parse storage-observation contract vocabulary. -/
def fromContract? : String -> Option StorageObservation
  | "noMutation" => some .noMutation
  | "inFlight" => some .inFlight
  | "successAcknowledged" => some .successAcknowledged
  | "mutationFailed" => some .mutationFailed
  | "staleObserved" => some .staleObserved
  | "readVisible" => some .readVisible
  | "lostAcknowledged" => some .lostAcknowledged
  | _ => none

theorem fromContract_toContract (obs : StorageObservation) :
    fromContract? obs.toContract = some obs := by
  cases obs <;> rfl

instance : HasTerminal StorageObservation where
  isTerminal obs := obs = .readVisible ∨ obs = .lostAcknowledged
  isTerminal_dec obs :=
    match obs with
    | .readVisible => isTrue (Or.inl rfl)
    | .lostAcknowledged => isTrue (Or.inr rfl)
    | .noMutation => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))
    | .inFlight => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))
    | .successAcknowledged => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))
    | .mutationFailed => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))
    | .staleObserved => isFalse (by intro h; cases h with
        | inl h => exact absurd h (by decide)
        | inr h => exact absurd h (by decide))

/-- Projection from daemon observations into the existing persistence model. -/
def toPersistence : StorageObservation -> PersistenceState
  | .noMutation => .uncommitted
  | .inFlight => .committing
  | .successAcknowledged => .committed
  | .mutationFailed => .uncommitted
  -- Stale observations only occur after a success ack in this model, so they
  -- preserve the daemon's committed observation instead of proving storage
  -- engine visibility.
  | .staleObserved => .committed
  | .readVisible => .committed
  | .lostAcknowledged => .lost

/-- Storage-observation actions. -/
inductive Action where
  | beginMutation
  | mutationSuccess
  | mutationFailure
  | staleRead
  | staleEvent
  | readYourWrites
  | eventArrives
  | retryFailClosed
  | acknowledgeLost
  deriving DecidableEq, Repr

/-- Relational storage-observation transitions parameterized by failure policy. -/
inductive Transition (policy : PersistenceState.FailurePolicy) :
    StorageObservation -> StorageObservation -> Prop where
  | begin_mutation :
      Transition policy .noMutation .inFlight
  | mutation_success :
      Transition policy .inFlight .successAcknowledged
  | mutation_failure :
      Transition policy .inFlight .mutationFailed
  | stale_read :
      Transition policy .successAcknowledged .staleObserved
  | stale_event :
      Transition policy .successAcknowledged .staleObserved
  | read_your_writes :
      Transition policy .successAcknowledged .readVisible
  | event_arrives_after_success :
      Transition policy .successAcknowledged .readVisible
  | event_arrives_after_stale :
      Transition policy .staleObserved .readVisible
  | retry_fail_closed :
      policy = .failClosed ->
      Transition policy .mutationFailed .noMutation
  | acknowledge_lost :
      policy = .failOpen ->
      Transition policy .mutationFailed .lostAcknowledged

/-- Executable storage-observation transition function. -/
def step? (policy : PersistenceState.FailurePolicy)
    (pre : StorageObservation) : Action -> Option StorageObservation
  | .beginMutation =>
      if pre = .noMutation then some .inFlight else none
  | .mutationSuccess =>
      if pre = .inFlight then some .successAcknowledged else none
  | .mutationFailure =>
      if pre = .inFlight then some .mutationFailed else none
  | .staleRead =>
      if pre = .successAcknowledged then some .staleObserved else none
  | .staleEvent =>
      if pre = .successAcknowledged then some .staleObserved else none
  | .readYourWrites =>
      if pre = .successAcknowledged then some .readVisible else none
  | .eventArrives =>
      if pre = .successAcknowledged ∨ pre = .staleObserved then some .readVisible else none
  | .retryFailClosed =>
      if policy = .failClosed /\ pre = .mutationFailed then some .noMutation else none
  | .acknowledgeLost =>
      if policy = .failOpen /\ pre = .mutationFailed then some .lostAcknowledged else none

/-- A trace is a sequence of valid storage observations. -/
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

theorem stale_observation_preserves_success_commit :
    toPersistence .staleObserved = toPersistence .successAcknowledged := rfl

/-- Downstream bridge predicate: a terminal response write may rely on either
    the local success ack or a later visible read observation. -/
def terminalWriteObserved (obs : StorageObservation) : Prop :=
  obs = .successAcknowledged ∨ obs = .readVisible

theorem terminal_write_observed_committed
    {obs : StorageObservation}
    (h_obs : terminalWriteObserved obs) :
    toPersistence obs = .committed := by
  cases h_obs with
  | inl h =>
      rw [h]
      rfl
  | inr h =>
      rw [h]
      rfl

theorem readYourWrites_visibility_path
    {policy : PersistenceState.FailurePolicy} :
    ∃ post : StorageObservation,
      Trace policy .successAcknowledged post ∧ post = .readVisible := by
  exact
    ⟨ .readVisible
    , Trace.step (Transition.read_your_writes (policy := policy)) Trace.refl
    , rfl
    ⟩

theorem successful_mutation_eventual_visibility_path
    {policy : PersistenceState.FailurePolicy} :
    ∃ post : StorageObservation,
      Trace policy .noMutation post ∧ post = .readVisible := by
  exact
    ⟨ .readVisible
    , Trace.step
        (Transition.begin_mutation (policy := policy))
        (Trace.step
          (Transition.mutation_success (policy := policy))
          (Trace.step (Transition.read_your_writes (policy := policy)) Trace.refl))
    , rfl
    ⟩

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

theorem staleRead_eventual_visibility_path
    {policy : PersistenceState.FailurePolicy} :
    ∃ post : StorageObservation,
      Trace policy .successAcknowledged post ∧ post = .readVisible := by
  exact
    ⟨ .readVisible
    , Trace.step
        (Transition.stale_read (policy := policy))
        (Trace.step (Transition.event_arrives_after_stale (policy := policy)) Trace.refl)
    , rfl
    ⟩

theorem staleEvent_eventual_visibility_path
    {policy : PersistenceState.FailurePolicy} :
    ∃ post : StorageObservation,
      Trace policy .successAcknowledged post ∧ post = .readVisible := by
  exact
    ⟨ .readVisible
    , Trace.step
        (Transition.stale_event (policy := policy))
        (Trace.step (Transition.event_arrives_after_stale (policy := policy)) Trace.refl)
    , rfl
    ⟩

end StorageObservation
