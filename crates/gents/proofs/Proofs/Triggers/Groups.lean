import Proofs.Triggers.Types

/-!
Run-scoped event-trigger fan-in.

This model is deliberately separate from the older concurrency model's
`TriggerKey`, which abstracts the pre-correlation runtime.  A group marker is
identified by every persisted discriminator used by production: target DID,
trigger id, trigger kind, and correlation.  `reconcile` models the
single-writer, query-then-create critical section.  Store visibility and fair
rescans remain explicit environment assumptions rather than being smuggled
into an "exactly once" claim.
-/

namespace Triggers.Groups

structure CorrelatedTriggerKey where
  targetAgentDid : String
  triggerId : String
  triggerKind : TriggerKind
  correlation : String
  deriving DecidableEq, Repr

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
