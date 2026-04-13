import Proofs.Basic
import Mathlib.Data.Finset.Basic

/-!
# Runtime Reconciliation Model

Models runtime reconciliation with the explicit control-plane and consumer
observation boundaries exposed by the live runtime:

1. A config write is acknowledged externally
2. A control event is observed and the changed document is read directly
3. The resolver makes a new `ResolvedSnapshot` visible
4. The supervisor publishes a new `ActiveRuntimeSnapshot`
5. The router observes that generation
6. Only then may a request be admitted on that generation

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

/-- Desired runtime state resolved from DefraDB documents. -/
structure ResolvedSnapshot where
  defaultBehavior : BehaviorId
  runnable : Finset BehaviorId
  unavailable : Finset BehaviorId
  deriving DecidableEq

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

/-- Runtime reconciliation, control-plane visibility, and request admission
    transitions. -/
inductive Transition : RuntimeState → RuntimeState → Prop where
  | ack_write {pre post : RuntimeState} (resolved : ResolvedSnapshot) :
      post = { pre with ackedResolved := some resolved } →
      Transition pre post
  | observe_doc {pre post : RuntimeState} (resolved : ResolvedSnapshot) :
      pre.ackedResolved = some resolved →
      pre.pendingResolved = none →
      post = { pre with phase := .debouncing, observedResolved := some resolved } →
      Transition pre post
  | start_resolve {pre post : RuntimeState} :
      pre.phase = .debouncing →
      post = { pre with phase := .resolving } →
      Transition pre post
  | resolve_visible {pre post : RuntimeState} (resolved : ResolvedSnapshot) :
      pre.phase = .resolving →
      pre.observedResolved = some resolved →
      resolved.wellFormed →
      post = { pre with phase := .diffing, pendingResolved := some resolved } →
      Transition pre post
  | diff_noop {pre post : RuntimeState} (resolved : ResolvedSnapshot) :
      pre.phase = .diffing →
      pre.pendingResolved = some resolved →
      resolved = pre.lastResolved →
      post = { pre with phase := .idle, pendingResolved := none } →
      Transition pre post
  | begin_apply {pre post : RuntimeState} (resolved : ResolvedSnapshot) :
      pre.phase = .diffing →
      pre.pendingResolved = some resolved →
      resolved ≠ pre.lastResolved →
      post = { pre with phase := .applying } →
      Transition pre post
  | publish {pre post : RuntimeState} (resolved : ResolvedSnapshot) :
      pre.phase = .applying →
      pre.pendingResolved = some resolved →
      resolved ≠ pre.lastResolved →
      post =
        { pre with
          phase := .idle
        , lastResolved := resolved
        , pendingResolved := none
        , active := resolved.activate (pre.active.generation + 1)
        , readyGenerations := insert (pre.active.generation + 1) pre.readyGenerations
        , liveGenerations := insert (pre.active.generation + 1) pre.liveGenerations
        } →
      Transition pre post
  | apply_failed {pre post : RuntimeState} :
      pre.phase = .applying →
      post = { pre with phase := .idle, pendingResolved := none } →
      Transition pre post
  | router_observe {pre post : RuntimeState} :
      pre.active.generation ∈ pre.readyGenerations →
      post = { pre with routerObservedGeneration := pre.active.generation } →
      Transition pre post
  | admit_request {pre post : RuntimeState} (sessionId : SessionId) (requestId : RequestId) :
      CanAdmitRequest pre sessionId requestId →
      post =
        { pre with
          inFlight := insert requestId pre.inFlight
        , requestGeneration := Function.update pre.requestGeneration requestId pre.routerObservedGeneration
        , requestSession := Function.update pre.requestSession requestId sessionId
        , requestBehavior := Function.update pre.requestBehavior requestId (pre.selectedBehavior sessionId)
        , sessionBehavior := pre.bindSessionIfNeeded sessionId (pre.selectedBehavior sessionId)
        } →
      Transition pre post
  | finish_request {pre post : RuntimeState} (requestId : RequestId) :
      requestId ∈ pre.inFlight →
      post = { pre with inFlight := pre.inFlight.erase requestId } →
      Transition pre post
  | retire_generation {pre post : RuntimeState} (generation : Generation) :
      generation ∈ pre.liveGenerations →
      generation ≠ pre.active.generation →
      generation ≠ pre.routerObservedGeneration →
      (∀ rid, rid ∈ pre.inFlight → pre.requestGeneration rid ≠ generation) →
      post =
        { pre with
          liveGenerations := pre.liveGenerations.erase generation
        , readyGenerations := pre.readyGenerations.erase generation
        } →
      Transition pre post

theorem transition_generation_monotone
    {pre post : RuntimeState}
    (h_trans : Transition pre post) :
    pre.active.generation ≤ post.active.generation := by
  cases h_trans with
  | ack_write _ h_post =>
      cases h_post
      simp
  | observe_doc _ _ _ h_post =>
      cases h_post
      simp
  | start_resolve _ h_post =>
      cases h_post
      simp
  | resolve_visible _ _ _ _ h_post =>
      cases h_post
      simp
  | diff_noop _ _ _ _ h_post =>
      cases h_post
      simp
  | begin_apply _ _ _ _ h_post =>
      cases h_post
      simp
  | publish _ _ _ _ h_post =>
      cases h_post
      simp [ResolvedSnapshot.activate, Nat.le_succ pre.active.generation]
  | apply_failed _ h_post =>
      cases h_post
      simp
  | router_observe _ h_post =>
      cases h_post
      simp
  | admit_request _ _ _ h_post =>
      cases h_post
      simp
  | finish_request _ _ h_post =>
      cases h_post
      simp
  | retire_generation _ _ _ _ _ h_post =>
      cases h_post
      simp

theorem coherent_preserved
    {pre post : RuntimeState}
    (h_coherent : pre.coherent)
    (h_trans : Transition pre post) :
    post.coherent := by
  rcases h_coherent with
    ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
      h_generation_ready, h_router_live, h_ready_live, h_live_bound,
      h_pending, h_request_live, h_session⟩
  cases h_trans with
  | ack_write _ h_post =>
      cases h_post
      exact ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound,
        h_pending, h_request_live, h_session⟩
  | observe_doc resolved _ h_pending_none h_post =>
      cases h_post
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound, ?_, h_request_live, h_session⟩
      intro candidate h_candidate
      simp [h_pending_none] at h_candidate
  | start_resolve _ h_post =>
      cases h_post
      exact ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound,
        h_pending, h_request_live, h_session⟩
  | resolve_visible resolved _ h_observed h_resolved h_post =>
      cases h_post
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound,
        ?_, h_request_live, h_session⟩
      intro candidate h_candidate
      simp at h_candidate
      cases h_candidate
      exact ⟨h_observed, h_resolved⟩
  | diff_noop _ _ _ _ h_post =>
      cases h_post
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound, ?_, h_request_live, h_session⟩
      intro candidate h_candidate
      simp at h_candidate
  | begin_apply _ _ _ _ h_post =>
      cases h_post
      exact ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound,
        h_pending, h_request_live, h_session⟩
  | publish resolved _ h_pendingResolved _ h_post =>
      cases h_post
      have h_resolved : resolved.wellFormed := (h_pending resolved h_pendingResolved).2
      refine ⟨activate_wellFormed h_resolved (Nat.succ_pos _), h_resolved, rfl, rfl, rfl, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
      · simp [ResolvedSnapshot.activate]
      · simp [ResolvedSnapshot.activate]
      · exact Finset.mem_insert_of_mem h_router_live
      · intro generation h_generation
        simp at h_generation
        rcases h_generation with h_new | h_old
        · simp [h_new, ResolvedSnapshot.activate]
        · exact Finset.mem_insert_of_mem (h_ready_live generation h_old)
      · intro generation h_generation
        simp at h_generation
        rcases h_generation with h_new | h_old
        · simp [h_new, ResolvedSnapshot.activate]
        · have h_old_bound := h_live_bound generation h_old
          exact Nat.le_trans h_old_bound (Nat.le_succ _)
      · intro candidate h_candidate
        simp at h_candidate
      · intro rid h_rid
        exact Finset.mem_insert_of_mem (h_request_live rid h_rid)
      · intro rid h_rid
        exact h_session rid h_rid
  | apply_failed _ h_post =>
      cases h_post
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound,
        ?_, h_request_live, h_session⟩
      intro candidate h_candidate
      simp at h_candidate
  | router_observe h_ready h_post =>
      cases h_post
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, ?_, h_ready_live, h_live_bound, h_pending, h_request_live, h_session⟩
      exact h_ready_live _ h_ready
  | admit_request sessionId requestId h_can h_post =>
      cases h_post
      rcases h_can with ⟨h_fresh, h_router_eq, h_router_ready, _h_dispatch⟩
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound, h_pending, ?_, ?_⟩
      · intro rid h_rid
        simp at h_rid
        rcases h_rid with rfl | h_old
        · have h_live : pre.routerObservedGeneration ∈ pre.liveGenerations :=
            h_ready_live _ h_router_ready
          simpa [Function.update] using h_live
        · have h_ne : rid ≠ requestId := by
            intro h_eq
            subst h_eq
            exact h_fresh h_old
          simpa [Function.update, h_ne] using h_request_live rid h_old
      · intro rid h_rid
        simp at h_rid
        rcases h_rid with rfl | h_old
        · simpa [Function.update] using bindSessionIfNeeded_selected pre sessionId
        · have h_ne : rid ≠ requestId := by
            intro h_eq
            subst h_eq
            exact h_fresh h_old
          by_cases h_same : pre.requestSession rid = sessionId
          · have h_bound : pre.sessionBehavior sessionId = some (pre.requestBehavior rid) := by
              simpa [h_same] using h_session rid h_old
            have h_bind_eq :
                pre.bindSessionIfNeeded sessionId (pre.selectedBehavior sessionId) =
                  pre.sessionBehavior :=
              bindSessionIfNeeded_eq_self_of_bound h_bound
            simpa [h_bind_eq, Function.update, h_ne, h_same]
              using h_session rid h_old
          · have h_other :
              pre.bindSessionIfNeeded sessionId (pre.selectedBehavior sessionId)
                  (pre.requestSession rid) =
                pre.sessionBehavior (pre.requestSession rid) :=
              bindSessionIfNeeded_other
                (s := pre)
                (sessionId := sessionId)
                (other := pre.requestSession rid)
                (behaviorId := pre.selectedBehavior sessionId)
                h_same
            simpa [h_other, Function.update, h_ne, h_same]
              using h_session rid h_old
  | finish_request _ _ h_post =>
      cases h_post
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, h_generation_live,
        h_generation_ready, h_router_live, h_ready_live, h_live_bound,
        h_pending, ?_, ?_⟩
      · intro rid h_rid
        exact h_request_live rid (Finset.mem_of_mem_erase h_rid)
      · intro rid h_rid
        exact h_session rid (Finset.mem_of_mem_erase h_rid)
  | retire_generation generation _ h_not_active h_not_router h_clear h_post =>
      cases h_post
      refine ⟨h_active, h_last, h_default, h_runnable, h_unavailable, ?_, ?_, ?_, ?_, ?_,
        h_pending, ?_, h_session⟩
      · have h_keep : pre.active.generation ≠ generation := by
          intro h_eq
          exact h_not_active h_eq.symm
        exact Finset.mem_erase.mpr ⟨h_keep, h_generation_live⟩
      · have h_keep : pre.active.generation ≠ generation := by
          intro h_eq
          exact h_not_active h_eq.symm
        exact Finset.mem_erase.mpr ⟨h_keep, h_generation_ready⟩
      · have h_keep : pre.routerObservedGeneration ≠ generation := by
          intro h_eq
          exact h_not_router h_eq.symm
        exact Finset.mem_erase.mpr ⟨h_keep, h_router_live⟩
      · intro current h_current
        rcases Finset.mem_erase.mp h_current with ⟨h_ne, h_mem⟩
        exact Finset.mem_erase.mpr ⟨h_ne, h_ready_live current h_mem⟩
      · intro current h_current
        exact h_live_bound current (Finset.mem_of_mem_erase h_current)
      · intro rid h_rid
        exact Finset.mem_erase.mpr ⟨h_clear rid h_rid, h_request_live rid h_rid⟩

end RuntimeState
