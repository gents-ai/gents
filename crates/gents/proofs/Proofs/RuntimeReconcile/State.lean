import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# Runtime Reconciliation Model

Models runtime reconciliation with the explicit control-plane and consumer
observation boundaries exposed by the live runtime:

1. A config write is acknowledged externally
2. A control event is observed and the changed document is read directly
3. The resolver makes a new `ResolvedSnapshot` visible
4. The supervisor publishes a new `ActiveRuntimeSnapshot`
5. The router observes that generation
6. Only then may a request be accepted on that generation

This keeps the storage/event consistency boundary explicit instead of assuming
that an acknowledged write is already visible to every secondary-index query.
-/

/-- Published runtime generation number. -/
abbrev Generation := Nat

/-- Reconcile control-plane phase. -/
inductive ReconcilePhase where
  | idle
  | debouncing
  | resolving
  | diffing
  | applying
  deriving DecidableEq, Repr

namespace ReconcilePhase

/-- String vocabulary persisted in `AgentRuntime.reconcile_phase`. -/
def toDefraDB : ReconcilePhase → String
  | .idle => "idle"
  | .debouncing => "debouncing"
  | .resolving => "resolving"
  | .diffing => "diffing"
  | .applying => "applying"

/-- Parse the persisted `AgentRuntime.reconcile_phase` vocabulary. -/
def fromDefraDB? : String → Option ReconcilePhase
  | "idle" => some .idle
  | "debouncing" => some .debouncing
  | "resolving" => some .resolving
  | "diffing" => some .diffing
  | "applying" => some .applying
  | _ => none

theorem fromDefraDB_toDefraDB (phase : ReconcilePhase) :
    fromDefraDB? phase.toDefraDB = some phase := by
  cases phase <;> rfl

end ReconcilePhase

/-- Desired runtime state resolved from DefraDB documents. -/
structure ResolvedSnapshot where
  defaultBehavior : BehaviorId
  runnable : Finset BehaviorId
  unavailable : Finset BehaviorId
  deriving DecidableEq

instance : Repr ResolvedSnapshot where
  reprPrec s _ :=
    "{ defaultBehavior := " ++ repr s.defaultBehavior ++
      ", runnableCard := " ++ repr s.runnable.card ++
      ", unavailableCard := " ++ repr s.unavailable.card ++ " }"

namespace ResolvedSnapshot

/-- Runnable and unavailable behavior sets must not overlap. -/
def wellFormed (s : ResolvedSnapshot) : Prop :=
  Disjoint s.runnable s.unavailable

instance (s : ResolvedSnapshot) : Decidable s.wellFormed := by
  unfold wellFormed
  infer_instance

end ResolvedSnapshot

/-- Published runtime state visible to the router and scheduler. -/
structure ActiveRuntimeSnapshot where
  generation : Generation
  defaultBehavior : BehaviorId
  runnable : Finset BehaviorId
  unavailable : Finset BehaviorId
  dispatchers : Finset BehaviorId
  deriving DecidableEq

instance : Repr ActiveRuntimeSnapshot where
  reprPrec s _ :=
    "{ generation := " ++ repr s.generation ++
      ", defaultBehavior := " ++ repr s.defaultBehavior ++
      ", runnableCard := " ++ repr s.runnable.card ++
      ", unavailableCard := " ++ repr s.unavailable.card ++
      ", dispatchersCard := " ++ repr s.dispatchers.card ++ " }"

namespace ActiveRuntimeSnapshot

/-- Published snapshots are valid only once their dispatchers exist. -/
def wellFormed (s : ActiveRuntimeSnapshot) : Prop :=
  0 < s.generation ∧
    s.dispatchers = s.runnable ∧
    Disjoint s.runnable s.unavailable

instance (s : ActiveRuntimeSnapshot) : Decidable s.wellFormed := by
  unfold wellFormed
  infer_instance

end ActiveRuntimeSnapshot

/-- Installing a resolved snapshot yields an active snapshot whose dispatch map
    exactly covers the runnable behavior set. -/
def ResolvedSnapshot.activate
    (resolved : ResolvedSnapshot)
    (generation : Generation) :
    ActiveRuntimeSnapshot :=
  { generation := generation
  , defaultBehavior := resolved.defaultBehavior
  , runnable := resolved.runnable
  , unavailable := resolved.unavailable
  , dispatchers := resolved.runnable
  }

theorem activate_wellFormed
    {resolved : ResolvedSnapshot}
    {generation : Generation}
    (h_resolved : resolved.wellFormed)
    (h_generation : 0 < generation) :
    (resolved.activate generation).wellFormed := by
  refine ⟨h_generation, rfl, ?_⟩
  simpa [ResolvedSnapshot.wellFormed] using h_resolved

/-- Full runtime state, including control-plane visibility and consumer
    observation boundaries. -/
structure RuntimeState where
  phase : ReconcilePhase
  ackedResolved : Option ResolvedSnapshot
  observedResolved : Option ResolvedSnapshot
  lastResolved : ResolvedSnapshot
  pendingResolved : Option ResolvedSnapshot
  active : ActiveRuntimeSnapshot
  routerObservedGeneration : Generation
  readyGenerations : Finset Generation
  liveGenerations : Finset Generation
  inFlight : Finset RequestId
  requestGeneration : RequestId → Generation
  requestSession : RequestId → SessionId
  requestBehavior : RequestId → BehaviorId
  sessionBehavior : SessionId → Option BehaviorId

namespace RuntimeState

/-- Session routing is behavior-scoped. Unbound sessions use the current default. -/
def selectedBehavior (s : RuntimeState) (sessionId : SessionId) : BehaviorId :=
  match s.sessionBehavior sessionId with
  | some behaviorId => behaviorId
  | none => s.active.defaultBehavior

/-- Bind a session to a behavior if it is not already bound. -/
def bindSessionIfNeeded
    (s : RuntimeState)
    (sessionId : SessionId)
    (behaviorId : BehaviorId) :
    SessionId → Option BehaviorId :=
  match s.sessionBehavior sessionId with
  | some _ => s.sessionBehavior
  | none => Function.update s.sessionBehavior sessionId (some behaviorId)

theorem bindSessionIfNeeded_selected
    (s : RuntimeState)
    (sessionId : SessionId) :
    s.bindSessionIfNeeded sessionId (s.selectedBehavior sessionId) sessionId =
      some (s.selectedBehavior sessionId) := by
  unfold bindSessionIfNeeded selectedBehavior
  cases h : s.sessionBehavior sessionId with
  | none =>
      simp [h, Function.update]
  | some behaviorId =>
      simp [h]

theorem bindSessionIfNeeded_other
    {s : RuntimeState}
    {sessionId other : SessionId}
    {behaviorId : BehaviorId}
    (h_other : other ≠ sessionId) :
    s.bindSessionIfNeeded sessionId behaviorId other = s.sessionBehavior other := by
  unfold bindSessionIfNeeded
  cases h : s.sessionBehavior sessionId <;> simp [h, Function.update, h_other]

theorem bindSessionIfNeeded_eq_self_of_bound
    {s : RuntimeState}
    {sessionId : SessionId}
    {bound requested : BehaviorId}
    (h_bound : s.sessionBehavior sessionId = some bound) :
    s.bindSessionIfNeeded sessionId requested = s.sessionBehavior := by
  funext sid
  unfold bindSessionIfNeeded
  simp [h_bound]

/-- Successful admission requires the router to have observed the currently
    active generation. -/
def CanAdmitRequest
    (s : RuntimeState)
    (sessionId : SessionId)
    (requestId : RequestId) : Prop :=
  requestId ∉ s.inFlight ∧
    s.routerObservedGeneration = s.active.generation ∧
    s.routerObservedGeneration ∈ s.readyGenerations ∧
    s.selectedBehavior sessionId ∈ s.active.dispatchers

instance
    (s : RuntimeState)
    (sessionId : SessionId)
    (requestId : RequestId) :
    Decidable (CanAdmitRequest s sessionId requestId) := by
  unfold CanAdmitRequest
  infer_instance

/-- Global runtime invariant for reconcile publication, consumer observation,
    and request bindings. -/
def coherent (s : RuntimeState) : Prop :=
  s.active.wellFormed ∧
    s.lastResolved.wellFormed ∧
    s.active.defaultBehavior = s.lastResolved.defaultBehavior ∧
    s.active.runnable = s.lastResolved.runnable ∧
    s.active.unavailable = s.lastResolved.unavailable ∧
    s.active.generation ∈ s.liveGenerations ∧
    s.active.generation ∈ s.readyGenerations ∧
    s.routerObservedGeneration ∈ s.liveGenerations ∧
    (∀ generation, generation ∈ s.readyGenerations → generation ∈ s.liveGenerations) ∧
    (∀ generation, generation ∈ s.liveGenerations → generation ≤ s.active.generation) ∧
    (∀ resolved,
      s.pendingResolved = some resolved →
        s.observedResolved = some resolved ∧ resolved.wellFormed) ∧
    (∀ rid, rid ∈ s.inFlight → s.requestGeneration rid ∈ s.liveGenerations) ∧
    (∀ rid, rid ∈ s.inFlight →
      s.sessionBehavior (s.requestSession rid) = some (s.requestBehavior rid))

/-- Initial startup installation publishes and exposes generation `1`. -/
def bootState (resolved : ResolvedSnapshot) : RuntimeState :=
  { phase := .idle
  , ackedResolved := none
  , observedResolved := none
  , lastResolved := resolved
  , pendingResolved := none
  , active := resolved.activate 1
  , routerObservedGeneration := 1
  , readyGenerations := {1}
  , liveGenerations := {1}
  , inFlight := ∅
  , requestGeneration := fun _ => 0
  , requestSession := fun _ => 0
  , requestBehavior := fun _ => resolved.defaultBehavior
  , sessionBehavior := fun _ => none
  }

theorem bootState_coherent
    {resolved : ResolvedSnapshot}
    (h_resolved : resolved.wellFormed) :
    (bootState resolved).coherent := by
  refine ⟨activate_wellFormed h_resolved (by decide), h_resolved, rfl, rfl, rfl, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · simp [bootState, ResolvedSnapshot.activate]
  · simp [bootState, ResolvedSnapshot.activate]
  · simp [bootState]
  · intro generation h_generation
    simp [bootState] at h_generation
    simpa [bootState] using h_generation
  · intro generation h_generation
    simp [bootState] at h_generation
    simp [h_generation, bootState, ResolvedSnapshot.activate]
  · intro candidate h_candidate
    simp [bootState] at h_candidate
  · intro rid h_rid
    simp [bootState] at h_rid
  · intro rid h_rid
    simp [bootState] at h_rid


end RuntimeState
