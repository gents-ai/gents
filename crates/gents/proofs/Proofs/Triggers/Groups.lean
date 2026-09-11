import Proofs.Triggers.Types

/-!
Shared durable event-group identity and clock.

`EventGroupState` replaces the trigger-only group-state vocabulary: one durable
first-seen/quiescence clock serves task triggers and callback bindings through
the existing event engine. The consumer is a typed trigger or callback-binding
reference under its `agent_did`. The durable group key derives from the owner,
consumer kind/id, effective delivery configuration key and correlation;
identical trigger and binding ids cannot alias because the consumer kind is
part of the key and of its derivation input, and a binding id is never placed
in a field whose semantics still say trigger id. Per-document and per-group
concurrency gating keep their existing scopes. `reconcile` models the
single-writer, query-then-create critical section. Store visibility and fair
rescans remain explicit environment assumptions rather than being smuggled
into an "exactly once" claim.
-/

namespace Triggers.Groups

def maxGroupDocs : Nat := 256

/-! Authored event grouping is shared by ordinary triggers and graph edges.
Resolved candidates below retain the existing deduplication/timeout owner. -/
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

/-- Dynamic counts resolve before the existing candidate eligibility check. -/
def ExpectedCount.resolve (e : ExpectedCount) (fields : String → Option Int) : Option Nat :=
  let value := match e with
    | .fixed n => some n
    | .sourceField field => fields field
  value.bind (fun n => if 0 < n then some n.toNat else none)

/-- Graph-only constraints narrow the common group; they do not copy its type. -/
def GroupConfig.validForGraph (g : GroupConfig) (correlation : String)
    (concurrency : ConcurrencyMode) : Bool :=
  g.valid correlation && concurrency != .latestOnly &&
    match g.expected with
    | none => false
    | some (.fixed n) => 2 <= n
    | some (.sourceField field) => !field.isEmpty

theorem empty_group_rejected (correlation : String) :
    (GroupConfig.mk none none none).valid correlation = false := by
  simp [GroupConfig.valid]

/-- Authored fixed counts fail shared admission before any delivery attempt. -/
theorem oversized_fixed_group_rejected (g : GroupConfig) (correlation : String) (count : Int)
    (he : g.expected = some (.fixed count)) (hb : (maxGroupDocs : Int) < count) :
    g.valid correlation = false := by
  have hn : ¬ count ≤ (maxGroupDocs : Int) := by omega
  simp [GroupConfig.valid, he, hn]

theorem missing_correlation_rejected (g : GroupConfig) : g.valid "" = false := by
  have h : "".isEmpty = true := rfl
  simp [GroupConfig.valid, h]

theorem latest_only_graph_rejected (g : GroupConfig) (correlation : String) :
    g.validForGraph correlation .latestOnly = false := by
  simp [GroupConfig.validForGraph]

theorem graph_validation_implies_shared (g : GroupConfig) (correlation : String)
    (mode : ConcurrencyMode) (h : g.validForGraph correlation mode = true) :
    g.valid correlation = true := by
  simp only [GroupConfig.validForGraph, Bool.and_eq_true] at h
  exact h.1.1

theorem fixed_nonpositive_rejected (n : Int) (h : n <= 0)
    (fields : String → Option Int) :
    ExpectedCount.resolve (.fixed n) fields = none := by
  simp [ExpectedCount.resolve, not_lt.mpr h]


/-!
Typed delivery consumer under its owner principal. Both consumer kinds share
the one durable first-seen/quiescence clock keyed by `EventGroupKey`; there is
no separate callback grouping clock and `CallbackResult` is not a clock.
-/

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

/-- Durable group identity: owner principal, typed consumer, effective delivery
configuration key, correlation. -/
structure EventGroupKey where
  agentDid : String
  consumer : EventConsumer
  consumerConfigKey : String
  correlation : String
  deriving DecidableEq, Repr

/-- Derivation input for the durable `group_key`. The consumer kind precedes
the raw id, so equal trigger and binding ids derive different keys. -/
def EventGroupKey.derivation (key : EventGroupKey) : List String :=
  [key.agentDid, key.consumer.kind, key.consumer.id,
    key.consumerConfigKey, key.correlation]

/-- The durable key derivation includes every discriminator, without ambiguous
string concatenation. Hashing/storage must preserve this structured encoding. -/
theorem derivation_injective (left right : EventGroupKey)
    (h : left.derivation = right.derivation) : left = right := by
  rcases left with ⟨la, lc, lf, lr⟩
  rcases right with ⟨ra, rc, rf, rr⟩
  cases lc <;> cases rc <;>
    simp_all [EventGroupKey.derivation, EventConsumer.kind, EventConsumer.id]

/-- Equal raw trigger and callback-binding ids under one owner, delivery
configuration and correlation name different durable groups. The derivation
never collapses a binding id into a field whose semantics say trigger id. -/
theorem equal_raw_ids_different_kinds_do_not_alias
    (agentDid configKey correlation rawId : String) :
    EventGroupKey.mk agentDid (.trigger rawId) configKey correlation ≠
      EventGroupKey.mk agentDid (.callbackBinding rawId) configKey correlation := by
  intro hEq
  exact EventConsumer.noConfusion (congrArg EventGroupKey.consumer hEq)

/-!
Concurrency gate identity. Per-document trigger delivery gates on the trigger
reference alone; per-group delivery and all callback-binding delivery gate on
the full typed group key. The old trigger-kind component is gone: `trigger_id`
is the logical trigger key within its `agent_did`, so the typed consumer plus
owner is the whole gate identity.
-/

structure TriggerWideKey where
  agentDid : String
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

def triggerGateKey (mode : FireMode) (agentDid triggerId configKey correlation : String) :
    ConcurrencyGateKey :=
  match mode with
  | .perDocument => .triggerWide { agentDid := agentDid, triggerId := triggerId }
  | .perGroup =>
      .correlated
        { agentDid := agentDid
        , consumer := .trigger triggerId
        , consumerConfigKey := configKey
        , correlation := correlation }

/-- Callback-binding delivery is group-scoped through the same gate vocabulary;
the binding never occupies a trigger-id field. -/
def bindingGateKey (agentDid bindingId configKey correlation : String) :
    ConcurrencyGateKey :=
  .correlated
    { agentDid := agentDid
    , consumer := .callbackBinding bindingId
    , consumerConfigKey := configKey
    , correlation := correlation }

theorem per_document_gate_ignores_correlation
    (agent trigger config a b : String) :
    triggerGateKey .perDocument agent trigger config a =
      triggerGateKey .perDocument agent trigger config b := rfl

theorem per_group_different_correlation_is_different_gate_scope
    (agentDid triggerId configKey correlation correlation' : String)
    (h : correlation ≠ correlation') :
    triggerGateKey .perGroup agentDid triggerId configKey correlation ≠
      triggerGateKey .perGroup agentDid triggerId configKey correlation' := by
  intro hEq
  have hKey := ConcurrencyGateKey.correlated.inj hEq
  exact h (congrArg EventGroupKey.correlation hKey)

/-- Equal raw ids keep distinct concurrency scopes across consumer kinds. -/
theorem equal_raw_ids_different_gate_scopes
    (agentDid configKey correlation rawId : String) :
    triggerGateKey .perGroup agentDid rawId configKey correlation ≠
      bindingGateKey agentDid rawId configKey correlation := by
  intro hEq
  have hKey := ConcurrencyGateKey.correlated.inj hEq
  exact equal_raw_ids_different_kinds_do_not_alias agentDid configKey correlation rawId hKey


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

/-- Resolve authored counts into the existing runtime candidate. A malformed
source count is an error, never silently converted into a timeout-only group. -/
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

/-- Oversized source counts fail resolution rather than leaving a permanently
ineligible candidate in the grouping owner. -/
theorem oversized_source_count_rejected (config : GroupConfig) (field correlationField : String)
    (key : EventGroupKey) (fields : String → Option Int) (actual : Nat) (timedOut : Bool)
    (value : Int) (he : config.expected = some (.sourceField field))
    (hf : fields field = some value) (hv : (maxGroupDocs : Int) < value) :
    resolveCandidate config correlationField key fields actual timedOut = none := by
  have hp : 0 < value := by unfold maxGroupDocs at hv; omega
  have hb : ¬ value.toNat <= maxGroupDocs := by omega
  simp [resolveCandidate, he, ExpectedCount.resolve, hf, hp, hb]

/-- All successfully resolved expected counts share the ordinary trigger bound;
graph composition only needs to add its minimum cardinality. -/
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

/-- Durable group-state marker keyed by the typed group key. The same marker
owner materializes trigger and callback-binding delivery groups once. -/
structure MarkerState where
  materialized : List EventGroupKey
  deriving DecidableEq, Repr

/-!
Timeout eligibility reads an immutable durable first-seen clock shared by
trigger and callback-binding consumers. The process cache may contain the same
value or be absent after restart/capacity eviction; absence falls back to the
durable value and cannot change the deadline.
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

def MarkerState.has (state : MarkerState) (key : EventGroupKey) : Bool :=
  state.materialized.contains key

/--
Executable refinement of the process-local critical section.  Production
holds the key lock while querying the durable group state and creating the
delivery group.  The state changes only after a successful create.
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

theorem different_owner_is_not_suppressed_by_other_owner_marker
    (state : MarkerState) (candidate : Candidate)
    (hEligible : candidate.eligible = true)
    (hDistinct : ∀ k ∈ state.materialized, k.agentDid ≠ candidate.key.agentDid) :
    candidate.key ∈ (reconcile state candidate).materialized := by
  have hNot : candidate.key ∉ state.materialized := fun h =>
    hDistinct candidate.key h rfl
  have hAbsent : state.has candidate.key = false := by
    simp [MarkerState.has, hNot]
  exact eligible_unmarked_materializes state candidate hEligible hAbsent

end Triggers.Groups
