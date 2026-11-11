import Proofs.Triggers.Types

namespace Triggers.Groups

def maxGroupDocs : Nat := 256

inductive ExpectedCount where
  | fixed (count : Int)
  | sourceField (field : String)
  deriving DecidableEq, Repr

structure GroupConfig where
  expected : Option ExpectedCount := none
  timeoutSecs : Option Int := none
  minimumCount : Option Int := none
  deriving DecidableEq, Repr

def positiveOptional (n : Option Int) : Bool :=
  n.all (fun value => 0 < value)

def GroupConfig.valid (g : GroupConfig) (correlation : String) : Bool :=
  !correlation.isEmpty && (g.expected.isSome || g.timeoutSecs.isSome) &&
    positiveOptional g.timeoutSecs && 0 < g.minimumCount.getD 1 &&
    match g.expected with
    | none => true
    | some (.sourceField field) => !field.isEmpty
    | some (.fixed count) =>
      0 < count && count <= maxGroupDocs && g.minimumCount.getD 1 <= count

def ExpectedCount.resolve (e : ExpectedCount) (fields : String → Option Int) : Option Nat :=
  let value := match e with
    | .fixed n => some n
    | .sourceField field => fields field
  value.bind (fun n => if 0 < n then some n.toNat else none)

theorem empty_group_rejected (correlation : String) :
    (GroupConfig.mk none none none).valid correlation = false := by
  simp [GroupConfig.valid]

theorem oversized_fixed_group_rejected (g : GroupConfig) (correlation : String) (count : Int)
    (he : g.expected = some (.fixed count)) (hb : (maxGroupDocs : Int) < count) :
    g.valid correlation = false := by
  have hn : ¬ count ≤ (maxGroupDocs : Int) := by omega
  simp [GroupConfig.valid, he, hn]

theorem missing_correlation_rejected (g : GroupConfig) : g.valid "" = false := by
  have h : "".isEmpty = true := rfl
  simp [GroupConfig.valid, h]

theorem fixed_nonpositive_rejected (n : Int) (h : n <= 0)
    (fields : String → Option Int) :
    ExpectedCount.resolve (.fixed n) fields = none := by
  simp [ExpectedCount.resolve, not_lt.mpr h]

inductive EventConsumer where
  | trigger (triggerId : String)
  | callbackBinding (bindingId : String)
  deriving DecidableEq, Repr

def EventConsumer.id : EventConsumer → String
  | .trigger id => id
  | .callbackBinding id => id

def EventConsumer.kind : EventConsumer → String
  | .trigger _ => "trigger"
  | .callbackBinding _ => "callback_binding"

structure EventGroupKey where
  nodeDid : String
  consumer : EventConsumer
  consumerConfigKey : String
  correlation : String
  deriving DecidableEq, Repr

def EventGroupKey.derivation (key : EventGroupKey) : List String :=
  [key.nodeDid, key.consumer.kind, key.consumer.id,
    key.consumerConfigKey, key.correlation]

theorem derivation_injective (left right : EventGroupKey)
    (h : left.derivation = right.derivation) : left = right := by
  rcases left with ⟨la, lc, lf, lr⟩
  rcases right with ⟨ra, rc, rf, rr⟩
  cases lc <;> cases rc <;>
    simp_all [EventGroupKey.derivation, EventConsumer.kind, EventConsumer.id]

theorem equal_raw_ids_different_kinds_do_not_alias
    (nodeDid configKey correlation rawId : String) :
    EventGroupKey.mk nodeDid (.trigger rawId) configKey correlation ≠
      EventGroupKey.mk nodeDid (.callbackBinding rawId) configKey correlation := by
  intro hEq
  exact EventConsumer.noConfusion (congrArg EventGroupKey.consumer hEq)

structure TriggerWideKey where
  nodeDid : String
  triggerId : String
  deriving DecidableEq, Repr

inductive FireMode where
  | perDocument
  | perGroup
  deriving DecidableEq, Repr

inductive ConcurrencyGateKey where
  | triggerWide (key : TriggerWideKey)
  | correlated (key : EventGroupKey)
  deriving DecidableEq, Repr

def triggerGateKey (mode : FireMode) (nodeDid triggerId configKey correlation : String) :
    ConcurrencyGateKey :=
  match mode with
  | .perDocument => .triggerWide { nodeDid := nodeDid, triggerId := triggerId }
  | .perGroup =>
      .correlated
        { nodeDid := nodeDid
        , consumer := .trigger triggerId
        , consumerConfigKey := configKey
        , correlation := correlation }

def bindingGateKey (nodeDid bindingId configKey correlation : String) :
    ConcurrencyGateKey :=
  .correlated
    { nodeDid := nodeDid
    , consumer := .callbackBinding bindingId
    , consumerConfigKey := configKey
    , correlation := correlation }

theorem per_document_gate_ignores_correlation
    (node trigger config a b : String) :
    triggerGateKey .perDocument node trigger config a =
      triggerGateKey .perDocument node trigger config b := rfl

theorem per_group_different_correlation_is_different_gate_scope
    (nodeDid triggerId configKey correlation correlation' : String)
    (h : correlation ≠ correlation') :
    triggerGateKey .perGroup nodeDid triggerId configKey correlation ≠
      triggerGateKey .perGroup nodeDid triggerId configKey correlation' := by
  intro hEq
  have hKey := ConcurrencyGateKey.correlated.inj hEq
  exact h (congrArg EventGroupKey.correlation hKey)

theorem equal_raw_ids_different_gate_scopes
    (nodeDid configKey correlation rawId : String) :
    triggerGateKey .perGroup nodeDid rawId configKey correlation ≠
      bindingGateKey nodeDid rawId configKey correlation := by
  intro hEq
  have hKey := ConcurrencyGateKey.correlated.inj hEq
  exact equal_raw_ids_different_kinds_do_not_alias nodeDid configKey correlation rawId hKey


structure Candidate where
  key : EventGroupKey
  actualCount : Nat
  expectedCount : Option Nat
  minimumCount : Nat
  timedOut : Bool
  wellFormed : Bool
  deriving DecidableEq, Repr

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

def resolveCandidate (config : GroupConfig) (correlationField : String)
    (key : EventGroupKey) (fields : String → Option Int)
    (actual : Nat) (timedOut : Bool) : Option Candidate :=
  if config.valid correlationField then
    let make (expected : Option Nat) : Candidate :=
      ⟨key, actual, expected, (config.minimumCount.getD 1).toNat, timedOut, true⟩
    match config.expected with
    | none => some (make none)
    | some expected =>
      (expected.resolve fields).bind fun count =>
        if config.minimumCount.getD 1 <= count ∧ count <= maxGroupDocs then
          some (make (some count)) else none
  else none

theorem invalid_source_count_rejected (config : GroupConfig) (field correlationField : String)
    (key : EventGroupKey) (fields : String → Option Int) (actual : Nat) (timedOut : Bool)
    (he : config.expected = some (.sourceField field)) (hf : fields field = none) :
    resolveCandidate config correlationField key fields actual timedOut = none := by
  simp [resolveCandidate, he, ExpectedCount.resolve, hf]

theorem oversized_source_count_rejected (config : GroupConfig) (field correlationField : String)
    (key : EventGroupKey) (fields : String → Option Int) (actual : Nat) (timedOut : Bool)
    (value : Int) (he : config.expected = some (.sourceField field))
    (hf : fields field = some value) (hv : (maxGroupDocs : Int) < value) :
    resolveCandidate config correlationField key fields actual timedOut = none := by
  have hp : 0 < value := by unfold maxGroupDocs at hv; omega
  have hb : ¬ value.toNat <= maxGroupDocs := by omega
  simp [resolveCandidate, he, ExpectedCount.resolve, hf, hp, hb]

theorem resolved_expected_count_bounded (config : GroupConfig) (correlationField : String)
    (key : EventGroupKey) (fields : String → Option Int) (actual : Nat) (timedOut : Bool)
    (candidate : Candidate) (count : Nat)
    (h : resolveCandidate config correlationField key fields actual timedOut = some candidate)
    (hc : candidate.expectedCount = some count) : count ≤ maxGroupDocs := by
  unfold resolveCandidate at h
  split at h
  · cases he : config.expected with
    | none => simp [he] at h; subst candidate; simp at hc
    | some e =>
      cases hr : e.resolve fields with
      | none => simp [he, hr] at h
      | some n =>
        simp only [he, hr, Option.bind] at h
        split at h
        · next hv =>
          have hh := Option.some.inj h
          subst candidate
          simp only [Option.some.injEq] at hc
          exact hc ▸ hv.2
        · simp at h
  · simp at h

structure MarkerState where
  materialized : List EventGroupKey
  deriving DecidableEq, Repr

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

def MarkerState.has (state : MarkerState) (key : EventGroupKey) : Bool :=
  state.materialized.contains key

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

theorem different_owner_is_not_suppressed_by_other_owner_marker
    (state : MarkerState) (candidate : Candidate)
    (hEligible : candidate.eligible = true)
    (hDistinct : ∀ k ∈ state.materialized, k.nodeDid ≠ candidate.key.nodeDid) :
    candidate.key ∈ (reconcile state candidate).materialized := by
  have hNot : candidate.key ∉ state.materialized := fun h =>
    hDistinct candidate.key h rfl
  have hAbsent : state.has candidate.key = false := by
    simp [MarkerState.has, hNot]
  exact eligible_unmarked_materializes state candidate hEligible hAbsent

end Triggers.Groups
