import Proofs.Triggers.Types

/-!
Run-scoped event-trigger fan-in.

This model is deliberately separate from the older request-lifecycle
concurrency model's `TriggerKey`, which abstracts the pre-correlation runtime.
It does model the runtime's gate identity: per-document concurrency is scoped
to target DID + trigger id + kind, while per-group concurrency adds correlation.
A group marker is identified by every persisted discriminator used by
production. `reconcile` models the single-writer, query-then-create critical
section. Store visibility and fair rescans remain explicit environment
assumptions rather than being smuggled into an "exactly once" claim.
-/

namespace Triggers.Groups

structure CorrelatedTriggerKey where
  targetAgentDid : String
  triggerId : String
  triggerKind : TriggerKind
  correlation : String
  deriving DecidableEq, Repr

structure TriggerWideKey where
  targetAgentDid : String
  triggerId : String
  triggerKind : TriggerKind
  deriving DecidableEq, Repr

inductive FireMode where
  | perDocument
  | perGroup
  deriving DecidableEq, Repr

inductive ConcurrencyGateKey where
  | triggerWide (key : TriggerWideKey)
  | correlated (key : CorrelatedTriggerKey)
  deriving DecidableEq, Repr

def CorrelatedTriggerKey.triggerWide (key : CorrelatedTriggerKey) : TriggerWideKey :=
  { targetAgentDid := key.targetAgentDid
  , triggerId := key.triggerId
  , triggerKind := key.triggerKind
  }

def concurrencyGateKey
    (mode : FireMode) (key : CorrelatedTriggerKey) : ConcurrencyGateKey :=
  match mode with
  | .perDocument => .triggerWide key.triggerWide
  | .perGroup => .correlated key

@[simp] theorem per_document_concurrency_is_trigger_wide
    (key : CorrelatedTriggerKey) :
    concurrencyGateKey .perDocument key = .triggerWide key.triggerWide := rfl

@[simp] theorem per_group_concurrency_is_correlation_scoped
    (key : CorrelatedTriggerKey) :
    concurrencyGateKey .perGroup key = .correlated key := rfl

structure Candidate where
  key : CorrelatedTriggerKey
  actualCount : Nat
  expectedCount : Option Nat
  minimumCount : Nat
  timedOut : Bool
  wellFormed : Bool
  deriving DecidableEq, Repr

def maxGroupDocs : Nat := 256

def Candidate.complete (candidate : Candidate) : Bool :=
  candidate.expectedCount == some candidate.actualCount

def Candidate.eligible (candidate : Candidate) : Bool :=
  candidate.wellFormed &&
    candidate.actualCount > 0 &&
    candidate.actualCount <= maxGroupDocs &&
    match candidate.expectedCount with
    | some expected =>
        expected > 0 && expected <= maxGroupDocs && candidate.actualCount <= expected &&
          (candidate.actualCount == expected ||
            (candidate.timedOut && candidate.minimumCount <= candidate.actualCount))
    | none => candidate.timedOut && candidate.minimumCount <= candidate.actualCount

structure MarkerState where
  materialized : List CorrelatedTriggerKey
  deriving DecidableEq, Repr

/-!
Timeout eligibility reads an immutable durable first-seen clock.  The process
cache may contain the same value or be absent after restart/capacity eviction;
absence falls back to the durable value and cannot change the deadline.
-/

structure TimeoutObservation where
  durableFirstSeen : Nat
  cachedFirstSeen : Option Nat
  deriving DecidableEq, Repr

def TimeoutObservation.observedFirstSeen (observation : TimeoutObservation) : Nat :=
  observation.cachedFirstSeen.getD observation.durableFirstSeen

def TimeoutObservation.elapsed
    (observation : TimeoutObservation) (now timeout : Nat) : Bool :=
  observation.observedFirstSeen + timeout <= now

def TimeoutObservation.evictCache (observation : TimeoutObservation) : TimeoutObservation :=
  { observation with cachedFirstSeen := none }

theorem timeout_cache_eviction_preserves_deadline
    (observation : TimeoutObservation)
    (hConsistent : observation.cachedFirstSeen = none ∨
      observation.cachedFirstSeen = some observation.durableFirstSeen)
    (now timeout : Nat) :
    observation.evictCache.elapsed now timeout = observation.elapsed now timeout := by
  rcases hConsistent with hMissing | hPresent
  · simp [TimeoutObservation.evictCache, TimeoutObservation.elapsed,
      TimeoutObservation.observedFirstSeen, hMissing]
  · simp [TimeoutObservation.evictCache, TimeoutObservation.elapsed,
      TimeoutObservation.observedFirstSeen, hPresent]

theorem timeout_restart_preserves_deadline
    (firstSeen now timeout : Nat) :
    (TimeoutObservation.mk firstSeen none).elapsed now timeout =
      (TimeoutObservation.mk firstSeen (some firstSeen)).elapsed now timeout := by
  simp [TimeoutObservation.elapsed, TimeoutObservation.observedFirstSeen]

def MarkerState.has (state : MarkerState) (key : CorrelatedTriggerKey) : Bool :=
  state.materialized.contains key

/--
Executable refinement of the process-local critical section.  Production
holds the key lock while querying the durable AgentRequest marker and creating
the request.  The state changes only after a successful create.
-/
def reconcile (state : MarkerState) (candidate : Candidate) : MarkerState :=
  if candidate.eligible && !state.has candidate.key then
    { materialized := candidate.key :: state.materialized }
  else
    state

theorem ineligible_does_not_materialize
    (state : MarkerState) (candidate : Candidate)
    (h : candidate.eligible = false) :
    reconcile state candidate = state := by
  simp [reconcile, h]

theorem existing_marker_suppresses_duplicate
    (state : MarkerState) (candidate : Candidate)
    (h : state.has candidate.key = true) :
    reconcile state candidate = state := by
  have hMem : candidate.key ∈ state.materialized := by
    simpa [MarkerState.has] using h
  simp [reconcile, MarkerState.has, hMem]

theorem eligible_unmarked_materializes
    (state : MarkerState) (candidate : Candidate)
    (hEligible : candidate.eligible = true)
    (hAbsent : state.has candidate.key = false) :
    candidate.key ∈ (reconcile state candidate).materialized := by
  simp [reconcile, hEligible, hAbsent]

theorem reconcile_idempotent
    (state : MarkerState) (candidate : Candidate) :
    reconcile (reconcile state candidate) candidate = reconcile state candidate := by
  by_cases hEligible : candidate.eligible = true
  · by_cases hPresent : candidate.key ∈ state.materialized
    · simp [reconcile, MarkerState.has, hEligible, hPresent]
    · simp [reconcile, MarkerState.has, hEligible, hPresent]
  · have hEligibleFalse : candidate.eligible = false := by
      cases h : candidate.eligible <;> simp_all
    simp [reconcile, hEligibleFalse]

theorem different_target_did_is_different_key
    (left right : CorrelatedTriggerKey)
    (h : left.targetAgentDid ≠ right.targetAgentDid) :
    left ≠ right := by
  intro hEq
  exact h (congrArg CorrelatedTriggerKey.targetAgentDid hEq)

theorem different_correlation_is_different_key
    (left right : CorrelatedTriggerKey)
    (h : left.correlation ≠ right.correlation) :
    left ≠ right := by
  intro hEq
  exact h (congrArg CorrelatedTriggerKey.correlation hEq)

theorem per_document_correlation_does_not_change_concurrency_scope
    (left right : CorrelatedTriggerKey)
    (hDid : left.targetAgentDid = right.targetAgentDid)
    (hId : left.triggerId = right.triggerId)
    (hKind : left.triggerKind = right.triggerKind) :
    concurrencyGateKey .perDocument left = concurrencyGateKey .perDocument right := by
  cases left
  cases right
  simp_all [concurrencyGateKey, CorrelatedTriggerKey.triggerWide]

theorem per_group_different_correlation_is_different_concurrency_scope
    (left right : CorrelatedTriggerKey)
    (h : left.correlation ≠ right.correlation) :
    concurrencyGateKey .perGroup left ≠ concurrencyGateKey .perGroup right := by
  intro hScope
  have hKey : left = right := ConcurrencyGateKey.correlated.inj hScope
  exact h (congrArg CorrelatedTriggerKey.correlation hKey)

/--
Liveness is conditional: if a fair rescan eventually presents a durable,
eligible candidate while the store query reports no marker and request
creation succeeds, that reconciliation materializes its marker.
-/
theorem liveness_under_fair_successful_reconcile
    (state : MarkerState) (candidate : Candidate)
    (hEligible : candidate.eligible = true)
    (hVisibleAbsent : state.has candidate.key = false) :
    candidate.key ∈ (reconcile state candidate).materialized :=
  eligible_unmarked_materializes state candidate hEligible hVisibleAbsent

end Triggers.Groups
