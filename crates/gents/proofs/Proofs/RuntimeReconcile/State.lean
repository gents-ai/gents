import Proofs.Basic
import Proofs.Process
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

abbrev Generation := Nat

inductive ReconcilePhase where
  | idle
  | debouncing
  | resolving
  | diffing
  | applying
  deriving DecidableEq, Repr

namespace ReconcilePhase

def toDefraDB : ReconcilePhase → String
  | .idle => "idle"
  | .debouncing => "debouncing"
  | .resolving => "resolving"
  | .diffing => "diffing"
  | .applying => "applying"

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

structure ResolvedSnapshot where
  /-- Upstream chooser metadata; request admission always requires its own explicit selection. -/
  defaultBehavior : AgentId
  runnable : Finset AgentId
  unavailable : Finset AgentId
  /-- Agents whose declared runtime dependencies were present while the
  snapshot was resolved. An agent may be dependency-ready but unavailable
  for another reason; the converse is forbidden. -/
  dependenciesSatisfied : Finset AgentId
  deriving DecidableEq

instance : Repr ResolvedSnapshot where
  reprPrec s _ :=
    "{ defaultBehavior := " ++ repr s.defaultBehavior ++
      ", runnableCard := " ++ repr s.runnable.card ++
      ", unavailableCard := " ++ repr s.unavailable.card ++
      ", dependenciesSatisfiedCard := " ++ repr s.dependenciesSatisfied.card ++ " }"

namespace ResolvedSnapshot

def wellFormed (s : ResolvedSnapshot) : Prop :=
  Disjoint s.runnable s.unavailable ∧
    s.runnable ⊆ s.dependenciesSatisfied ∧
    s.defaultBehavior ∈ s.runnable ∪ s.unavailable

instance (s : ResolvedSnapshot) : Decidable s.wellFormed := by
  unfold wellFormed
  infer_instance

end ResolvedSnapshot

structure ActiveRuntimeSnapshot where
  generation : Generation
  defaultBehavior : AgentId
  runnable : Finset AgentId
  unavailable : Finset AgentId
  dependenciesSatisfied : Finset AgentId
  dispatchers : Finset AgentId
  deriving DecidableEq

instance : Repr ActiveRuntimeSnapshot where
  reprPrec s _ :=
    "{ generation := " ++ repr s.generation ++
      ", defaultBehavior := " ++ repr s.defaultBehavior ++
      ", runnableCard := " ++ repr s.runnable.card ++
      ", unavailableCard := " ++ repr s.unavailable.card ++
      ", dependenciesSatisfiedCard := " ++ repr s.dependenciesSatisfied.card ++
      ", dispatchersCard := " ++ repr s.dispatchers.card ++ " }"

namespace ActiveRuntimeSnapshot

def wellFormed (s : ActiveRuntimeSnapshot) : Prop :=
  0 < s.generation ∧
    s.dispatchers = s.runnable ∧
    Disjoint s.runnable s.unavailable ∧
    s.runnable ⊆ s.dependenciesSatisfied ∧
    s.defaultBehavior ∈ s.runnable ∪ s.unavailable

instance (s : ActiveRuntimeSnapshot) : Decidable s.wellFormed := by
  unfold wellFormed
  infer_instance

end ActiveRuntimeSnapshot

def ResolvedSnapshot.activate
    (resolved : ResolvedSnapshot)
    (generation : Generation) :
    ActiveRuntimeSnapshot :=
  { generation := generation
  , defaultBehavior := resolved.defaultBehavior
  , runnable := resolved.runnable
  , unavailable := resolved.unavailable
  , dependenciesSatisfied := resolved.dependenciesSatisfied
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

structure RuntimeState where
  phase : ReconcilePhase
  ackedResolved : Option ResolvedSnapshot
  observedResolved : Option ResolvedSnapshot
  lastResolved : ResolvedSnapshot
  pendingResolved : Option ResolvedSnapshot
  active : ActiveRuntimeSnapshot
  routerObservedGeneration : Generation
  /-- Agents removed from effective dispatch after exhausting the startup
  build budget. They remain in the immutable active snapshot for diagnostics,
  but both runtime admission and client readiness must treat them as unavailable. -/
  startupDemoted : Finset AgentId
  readyGenerations : Finset Generation
  liveGenerations : Finset Generation
  /-- Requests whose claim and request-owned session projection committed atomically. -/
  accepted : Finset RequestId
  inFlight : Finset RequestId
  requestGeneration : RequestId → Generation
  requestSession : RequestId → SessionId
  requestBehavior : RequestId → AgentId
  sessionBehavior : SessionId → Option AgentId

namespace RuntimeState

def effectiveDispatchers (s : RuntimeState) : Finset AgentId :=
  s.active.dispatchers \ s.startupDemoted

def effectiveUnavailable (s : RuntimeState) : Finset AgentId :=
  s.active.unavailable ∪ s.startupDemoted

def bindSessionIfNeeded
    (s : RuntimeState)
    (sessionId : SessionId)
    (agentId : AgentId) :
    SessionId → Option AgentId :=
  match s.sessionBehavior sessionId with
  | some _ => s.sessionBehavior
  | none => Function.update s.sessionBehavior sessionId (some agentId)

theorem bindSessionIfNeeded_requested
    (s : RuntimeState) (sessionId : SessionId) (requested : AgentId)
    (h : (s.sessionBehavior sessionId).all (· == requested) = true) :
    s.bindSessionIfNeeded sessionId requested sessionId = some requested := by
  unfold bindSessionIfNeeded
  cases hs : s.sessionBehavior sessionId <;> simp_all

theorem bindSessionIfNeeded_other
    {s : RuntimeState}
    {sessionId other : SessionId}
    {agentId : AgentId}
    (h_other : other ≠ sessionId) :
    s.bindSessionIfNeeded sessionId agentId other = s.sessionBehavior other := by
  unfold bindSessionIfNeeded
  cases h : s.sessionBehavior sessionId <;> simp [h, Function.update, h_other]

theorem bindSessionIfNeeded_eq_self_of_bound
    {s : RuntimeState}
    {sessionId : SessionId}
    {bound requested : AgentId}
    (h_bound : s.sessionBehavior sessionId = some bound) :
    s.bindSessionIfNeeded sessionId requested = s.sessionBehavior := by
  funext sid
  unfold bindSessionIfNeeded
  simp [h_bound]

def AgentAdmissible
    (process : ProcessState)
    (s : RuntimeState)
    (agentId : AgentId) : Prop :=
  process.acceptsWork ∧
    0 < s.active.generation ∧
    s.routerObservedGeneration = s.active.generation ∧
    agentId ∈ s.effectiveDispatchers ∧
    agentId ∉ s.effectiveUnavailable

instance
    (process : ProcessState)
    (s : RuntimeState)
    (agentId : AgentId) :
    Decidable (AgentAdmissible process s agentId) := by
  unfold AgentAdmissible
  infer_instance

def CanAdmitRequest
    (process : ProcessState)
    (s : RuntimeState)
    (sessionId : SessionId)
    (requestId : RequestId) (requested : AgentId) : Prop :=
  requestId ∉ s.accepted ∧
    requestId ∉ s.inFlight ∧
    AgentAdmissible process s requested ∧
    (s.sessionBehavior sessionId).all (· == requested) = true

instance
    (process : ProcessState)
    (s : RuntimeState)
    (sessionId : SessionId)
    (requestId : RequestId) (requested : AgentId) :
    Decidable (CanAdmitRequest process s sessionId requestId requested) := by
  unfold CanAdmitRequest
  infer_instance

/-- Client-facing readiness is derived from the same runtime state and
effective dispatcher set used by request admission. Configuration rows never
participate. Missing observations, a non-ready process, and generation skew
all fail closed. -/
inductive AgentUnavailableReason where
  | agentDisabled
  | runtimeConfigurationInvalid
  | backendNotConfigured
  | backendDisabled
  | backendTemporarilyUnavailable
  | credentialsRequired
  | inferenceProfileInvalid
  | toolConfigurationInvalid
  | toolSurfaceUnavailable
  deriving DecidableEq, Repr

namespace AgentUnavailableReason

def code : AgentUnavailableReason → String
  | .agentDisabled => "agent_disabled"
  | .runtimeConfigurationInvalid => "runtime_configuration_invalid"
  | .backendNotConfigured => "backend_not_configured"
  | .backendDisabled => "backend_disabled"
  | .backendTemporarilyUnavailable => "backend_temporarily_unavailable"
  | .credentialsRequired => "credentials_required"
  | .inferenceProfileInvalid => "inference_profile_invalid"
  | .toolConfigurationInvalid => "tool_configuration_invalid"
  | .toolSurfaceUnavailable => "tool_surface_unavailable"

end AgentUnavailableReason

inductive ClientAgentReadiness where
  | ready
  | unavailableRuntime (reason : AgentUnavailableReason)
  | unavailableStartup
  | unknownMissing
  | unknownMalformed
  | unknownVersion
  | unknownProcess
  | unknownStale
  | unknownAbsent
  deriving DecidableEq, Repr

namespace ClientAgentReadiness

def stateString : ClientAgentReadiness → String
  | .ready => "ready"
  | .unavailableRuntime _ | .unavailableStartup => "unavailable"
  | .unknownMissing | .unknownMalformed | .unknownVersion
  | .unknownProcess | .unknownStale | .unknownAbsent => "unknown"

def reasonCode : ClientAgentReadiness → Option String
  | .ready => none
  | .unavailableRuntime reason => some reason.code
  | .unavailableStartup => some "executor_start_failed"
  | .unknownMissing => some "readiness_missing"
  | .unknownMalformed => some "readiness_malformed"
  | .unknownVersion => some "readiness_version_unsupported"
  | .unknownProcess => some "process_not_ready"
  | .unknownStale => some "router_generation_stale"
  | .unknownAbsent => some "agent_not_assigned"

inductive ClientNodeObservation where
  | missing
  | malformed
  | unsupportedVersion
  | observed (process : ProcessState) (state : RuntimeState)

def project
    (observation : ClientNodeObservation)
    (agentId : AgentId)
    (runtimeUnavailableReason : AgentUnavailableReason) : ClientAgentReadiness :=
  match observation with
  | .missing => .unknownMissing
  | .malformed => .unknownMalformed
  | .unsupportedVersion => .unknownVersion
  | .observed process s =>
      if !decide process.acceptsWork then
        .unknownProcess
      else if s.active.generation = 0 ∨
        s.routerObservedGeneration ≠ s.active.generation then
        .unknownStale
      else if agentId ∈ s.startupDemoted then
        .unavailableStartup
      else if agentId ∈ s.active.unavailable then
        .unavailableRuntime runtimeUnavailableReason
      else if agentId ∈ s.effectiveDispatchers then
        .ready
      else
        .unknownAbsent

theorem ready_sound
    {process : ProcessState}
    {s : RuntimeState}
    {agentId : AgentId}
    (hReady : project (.observed process s) agentId .backendTemporarilyUnavailable = .ready) :
    AgentAdmissible process s agentId := by
  have hProcess : process.acceptsWork := by
    by_contra h
    simp [project, h] at hReady
  have hZero : s.active.generation ≠ 0 := by
    intro h
    simp [project, hProcess, h] at hReady
  have hGeneration : s.routerObservedGeneration = s.active.generation := by
    by_contra h
    simp [project, hProcess, hZero, h] at hReady
  have hDemoted : agentId ∉ s.startupDemoted := by
    intro h
    simp [project, hProcess, hZero, hGeneration, h] at hReady
  have hActiveUnavailable : agentId ∉ s.active.unavailable := by
    intro h
    simp [project, hProcess, hZero, hGeneration, hDemoted, h] at hReady
  have hDispatcher : agentId ∈ s.effectiveDispatchers := by
    by_cases h : agentId ∈ s.effectiveDispatchers
    · exact h
    · simp [project, hProcess, hZero, hGeneration, hDemoted, hActiveUnavailable, h] at hReady
  refine ⟨hProcess, Nat.pos_of_ne_zero hZero, hGeneration, hDispatcher, ?_⟩
  simp [effectiveUnavailable, hActiveUnavailable, hDemoted]

theorem missing_or_stale_never_ready
    {observation : ClientNodeObservation}
    {agentId : AgentId}
    (hClosed : observation = .missing ∨ observation = .malformed ∨
      observation = .unsupportedVersion ∨
      ∃ process s, observation = .observed process s ∧
        (s.active.generation = 0 ∨ s.routerObservedGeneration ≠ s.active.generation)) :
    project observation agentId .backendTemporarilyUnavailable ≠ .ready := by
  rcases hClosed with hMissing | hMalformed | hVersion |
    ⟨process, s, hObservation, hStale⟩
  · simp [project, hMissing]
  · simp [project, hMalformed]
  · simp [project, hVersion]
  · subst observation
    rcases hStale with hZero | hGeneration
    · cases process <;> simp [project, ProcessState.acceptsWork, hZero]
    · cases process <;> simp [project, ProcessState.acceptsWork, hGeneration]

theorem unavailable_wins_overlap
    {process : ProcessState}
    {s : RuntimeState}
    {agentId : AgentId}
    (hUnavailable : agentId ∈ s.effectiveUnavailable) :
    project (.observed process s) agentId .backendTemporarilyUnavailable ≠ .ready := by
  intro hReady
  rcases ready_sound hReady with ⟨_, _, _, _, hNotUnavailable⟩
  exact hNotUnavailable hUnavailable

theorem observed_ready_iff_admissible
    {process : ProcessState}
    {s : RuntimeState}
    {agentId : AgentId} :
    project (.observed process s) agentId .backendTemporarilyUnavailable = .ready ↔
      AgentAdmissible process s agentId := by
  constructor
  · exact ready_sound
  · intro hAdmissible
    rcases hAdmissible with
      ⟨hProcess, hPositive, hGeneration, hDispatcher, hUnavailable⟩
    have hZero : s.active.generation ≠ 0 := Nat.ne_of_gt hPositive
    have hDemoted : agentId ∉ s.startupDemoted := by
      intro h
      exact hUnavailable (by simp [effectiveUnavailable, h])
    have hActiveUnavailable : agentId ∉ s.active.unavailable := by
      intro h
      exact hUnavailable (by simp [effectiveUnavailable, h])
    simp [project, hProcess, hZero, hGeneration, hDemoted, hActiveUnavailable, hDispatcher]

theorem ready_implies_runtime_admission_when_fresh
    {process : ProcessState}
    {s : RuntimeState}
    {sessionId : SessionId}
    {requestId : RequestId}
    {requested : AgentId}
    (hBinding : (s.sessionBehavior sessionId).all (· == requested) = true)
    (hReady : project (.observed process s) requested .backendTemporarilyUnavailable = .ready)
    (hUnaccepted : requestId ∉ s.accepted)
    (hNotInFlight : requestId ∉ s.inFlight) :
    CanAdmitRequest process s sessionId requestId requested := by
  exact ⟨hUnaccepted, hNotInFlight, ready_sound hReady, hBinding⟩

end ClientAgentReadiness

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

/-- `readyGenerations` is an internal transition guard, not an independent
client-wire admission fact. In every coherent runtime state, an aligned router
necessarily observes the active ready generation. -/
theorem coherent_aligned_router_is_ready
    {s : RuntimeState}
    (hCoherent : s.coherent)
    (hAligned : s.routerObservedGeneration = s.active.generation) :
    s.routerObservedGeneration ∈ s.readyGenerations := by
  rcases hCoherent with ⟨_, _, _, _, _, _, hActiveReady, _, _, _, _, _, _⟩
  simpa [hAligned] using hActiveReady

def bootState (resolved : ResolvedSnapshot) : RuntimeState :=
  { phase := .idle
  , ackedResolved := none
  , observedResolved := none
  , lastResolved := resolved
  , pendingResolved := none
  , active := resolved.activate 1
  , routerObservedGeneration := 1
  , startupDemoted := ∅
  , readyGenerations := {1}
  , liveGenerations := {1}
  , accepted := ∅
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
