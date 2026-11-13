import Proofs.Shared.Basic
import Proofs.Automation.Process
import Proofs.Documents.Configuration
import Proofs.Session.PromptAssembly.ServiceTier
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

abbrev Generation := Nat

/-- The inference settings one generation binds to one agent.

`Configuration.SelectedModel` is the backend, model and effort produced by the
only legal selection path; the service tier is owned separately by
`PromptAssembly.ServiceTier`, because its legality is a provider capability
rather than a selection field. Joining them here keeps a single settings
vocabulary: the tier is not redeclared, and the selection does not gain a tier
field whose capability rule would then have to be restated in two places.

The claim stores this execution evidence on AgentRequest, using the existing
`delivered_requests` resource and AgentSession parent. Its create/read/update/delete
grants are unchanged; no new resource or permission is introduced. -/
structure EffectiveSettings where
  selected : Configuration.SelectedModel
  tier : Option PromptAssembly.ServiceTier.Tier
  deriving DecidableEq, Repr

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
  defaultAgent : AgentDid
  runnable : Finset AgentDid
  unavailable : Finset AgentDid
  /-- Agents whose declared runtime dependencies were present while the
  snapshot was resolved; dependency-ready but unavailable for another reason
  is allowed, the converse is not. -/
  dependenciesSatisfied : Finset AgentDid
  deriving DecidableEq

instance : Repr ResolvedSnapshot where
  reprPrec s _ :=
    "{ defaultAgent := " ++ repr s.defaultAgent ++
      ", runnableCard := " ++ repr s.runnable.card ++
      ", unavailableCard := " ++ repr s.unavailable.card ++
      ", dependenciesSatisfiedCard := " ++ repr s.dependenciesSatisfied.card ++ " }"

namespace ResolvedSnapshot

def wellFormed (s : ResolvedSnapshot) : Prop :=
  Disjoint s.runnable s.unavailable ∧
    s.runnable ⊆ s.dependenciesSatisfied ∧
    s.defaultAgent ∈ s.runnable ∪ s.unavailable

instance (s : ResolvedSnapshot) : Decidable s.wellFormed := by
  unfold wellFormed
  infer_instance

/-- A tool surface that cannot be built makes its agent unavailable before
publication. Other runnable agents and the default selection remain intact.
Native ACP determines whether the selected agent can read the WorkspaceRoot
that admits its configured host tools; the model receives the resulting
surface-build failure without inferring authority from another actor. -/
def demote (s : ResolvedSnapshot) (agent : AgentDid) : ResolvedSnapshot :=
  { s with runnable := s.runnable.erase agent, unavailable := insert agent s.unavailable }

theorem demote_preserves_other_agents (s : ResolvedSnapshot) (agent other : AgentDid)
    (h : other ≠ agent) :
    other ∈ (s.demote agent).runnable ↔ other ∈ s.runnable := by
  simp [demote, h]

theorem demote_is_unavailable (s : ResolvedSnapshot) (agent : AgentDid) :
    agent ∈ (s.demote agent).unavailable ∧ agent ∉ (s.demote agent).runnable := by
  simp [demote]

theorem demote_idempotent (s : ResolvedSnapshot) (agent : AgentDid) :
    (s.demote agent).demote agent = s.demote agent := by
  simp [demote]

theorem demote_wellFormed (s : ResolvedSnapshot) (agent : AgentDid)
    (h : s.wellFormed) : (s.demote agent).wellFormed := by
  rcases h with ⟨hDisjoint, hDependencies, hDefault⟩
  refine ⟨?_, ?_, ?_⟩
  · rw [Finset.disjoint_left]
    intro other hRunnable hUnavailable
    simp only [demote, Finset.mem_erase] at hRunnable
    simp only [demote, Finset.mem_insert] at hUnavailable
    rcases hUnavailable with rfl | hUnavailable
    · exact hRunnable.1 rfl
    · exact Finset.disjoint_left.mp hDisjoint hRunnable.2 hUnavailable
  · exact fun _ hRunnable => hDependencies (Finset.mem_erase.mp hRunnable).2
  · simp only [demote, Finset.mem_union, Finset.mem_erase, Finset.mem_insert]
    rcases Finset.mem_union.mp hDefault with hRunnable | hUnavailable
    · by_cases hSame : s.defaultAgent = agent
      · exact Or.inr (Or.inl hSame)
      · exact Or.inl ⟨hSame, hRunnable⟩
    · exact Or.inr (Or.inr hUnavailable)

end ResolvedSnapshot

structure ActiveRuntimeSnapshot where
  generation : Generation
  defaultAgent : AgentDid
  runnable : Finset AgentDid
  unavailable : Finset AgentDid
  dependenciesSatisfied : Finset AgentDid
  dispatchers : Finset AgentDid
  deriving DecidableEq

instance : Repr ActiveRuntimeSnapshot where
  reprPrec s _ :=
    "{ generation := " ++ repr s.generation ++
      ", defaultAgent := " ++ repr s.defaultAgent ++
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
    s.defaultAgent ∈ s.runnable ∪ s.unavailable

instance (s : ActiveRuntimeSnapshot) : Decidable s.wellFormed := by
  unfold wellFormed
  infer_instance

end ActiveRuntimeSnapshot

def ResolvedSnapshot.activate
    (resolved : ResolvedSnapshot)
    (generation : Generation) :
    ActiveRuntimeSnapshot :=
  { generation := generation
  , defaultAgent := resolved.defaultAgent
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
  build budget; both runtime admission and client readiness must treat them
  as unavailable. -/
  startupDemoted : Finset AgentDid
  readyGenerations : Finset Generation
  liveGenerations : Finset Generation
  /-- Requests whose claim and request-owned session projection committed atomically. -/
  accepted : Finset RequestId
  inFlight : Finset RequestId
  requestGeneration : RequestId → Generation
  requestSession : RequestId → SessionId
  requestAgent : RequestId → AgentDid
  sessionAgent : SessionId → Option AgentDid
  /-- The settings a generation's slot holds for an agent. A slot is built once
  per generation and keeps one immutable resolved configuration for its whole
  life, so this is a property of the generation, not of any request. -/
  generationSettings : Generation → AgentDid → Option EffectiveSettings
  /-- The settings a request's execution is bound to, recorded when the router
  hands the request to a slot. `none` is first class and means not yet bound:
  the request has no effective settings and will bind at its own dispatch. -/
  requestSettings : RequestId → Option EffectiveSettings

namespace RuntimeState

/-- Router observations are actor-scoped. No visible session may mean absent,
unavailable or unauthorized; the router waits within the bound of
`sessionSelectionDisposition` rather than inferring a new binding. -/
inductive SessionSelection where
  | unavailable | matched | mismatched
  deriving DecidableEq, Repr

def sessionSelection (requested : AgentDid) (visible : Option AgentDid) : SessionSelection :=
  match visible with
  | none => .unavailable
  | some bound => if bound = requested then .matched else .mismatched

theorem missing_session_selection_waits (requested : AgentDid) :
    sessionSelection requested none = .unavailable := rfl

theorem visible_foreign_session_is_rejected (requested bound : AgentDid)
    (different : bound ≠ requested) :
    sessionSelection requested (some bound) = .mismatched := by
  simp [sessionSelection, different]

/-- What the router does with one session-selection observation, given how many
consecutive unavailable observations it has made for the same request, this one
included.

External premises that justify terminalizing rather than waiting forever:
DefraDB document ACP filters unreadable rows without an error, so an
unavailable observation cannot be distinguished from an absent session;
sessions are node-local documents (P2P carries discovery and enrollment only),
so repeated local invisibility is not a replication race; and a request left
pending is re-served at every request-watcher cooldown, so an unbounded wait is
an unbounded retry that never progresses.

Only `unavailable` is bounded. A failed read is not an observation and must not
advance the count: `Enrollment.AgentRequestAdmissionDisposition` allows only an
authoritative negative to terminalize a durable request. -/
inductive SessionSelectionDisposition where
  | claim | wait | reject
  deriving DecidableEq, Repr

def sessionSelectionDisposition
    (selection : SessionSelection) (attempts bound : Nat) : SessionSelectionDisposition :=
  match selection with
  | .matched => .claim
  | .mismatched => .reject
  | .unavailable => if bound ≤ attempts then .reject else .wait

theorem unavailable_waits_below_bound {attempts bound : Nat} (below : attempts < bound) :
    sessionSelectionDisposition .unavailable attempts bound = .wait := by
  simp [sessionSelectionDisposition, Nat.not_le.mpr below]

theorem unavailable_rejects_at_bound {attempts bound : Nat} (reached : bound ≤ attempts) :
    sessionSelectionDisposition .unavailable attempts bound = .reject := by
  simp [sessionSelectionDisposition, reached]

theorem matched_never_rejects (attempts bound : Nat) :
    sessionSelectionDisposition .matched attempts bound = .claim := rfl

theorem mismatched_rejects_immediately (attempts bound : Nat) :
    sessionSelectionDisposition .mismatched attempts bound = .reject := rfl

/-- What a request's own Agent can read of its session when the router first
observes the request: the session's bound Agent if the producer published the
session with the request, nothing otherwise.

Decision (#3670, scheduled fires never ran): because the router waits on an
absent session for a bounded number of observations instead of binding one, every
producer of a runtime request (Task fires, manual Task runs) publishes the
request's session, bound to the request's Agent and owned by it, in the
request's own transaction. The claim projection then only updates that
session. A producer that left the session to the claim would leave its request
unroutable (`unpublished_session_never_routes`). -/
def materializedSessionVisible (publishedWithRequest : Bool) (requested : AgentDid) :
    Option AgentDid :=
  if publishedWithRequest then some requested else none

theorem materialized_request_is_routable (requested : AgentDid) :
    sessionSelection requested (materializedSessionVisible true requested) = .matched := by
  simp [materializedSessionVisible, sessionSelection]

theorem unpublished_session_never_routes (requested : AgentDid) :
    sessionSelection requested (materializedSessionVisible false requested) = .unavailable :=
  rfl

/-- Before claiming, the router reads the routed generation's selected
configuration as its Agent (`Identity.Configuration.providerConfigurationVisible`).
A committed configuration change can revoke that selection before the control
watcher activates the replacement generation; a claim would then fail the
request at its first provider-input assembly although the replacement could run
it. Invisibility may mean superseded or revoked, so, as with a missing session
selection, the router does not claim and the request waits for a newer active
generation. A revocation after the claim is refused by the assembly check; the
read and the claim are not atomic. -/
inductive ConfigurationAdmission where
  | claim | awaitNewerGeneration
  deriving DecidableEq, Repr

def configurationAdmission (visible : Bool) : ConfigurationAdmission :=
  if visible then .claim else .awaitNewerGeneration

/-- A request deferred under one generation is routed again only once a newer
generation is active; the deferring generation retains the same selection. -/
def retriesDeferred (deferredAt active : Generation) : Bool :=
  deferredAt < active

theorem invisible_configuration_is_not_claimed :
    configurationAdmission false = .awaitNewerGeneration := rfl

theorem visible_configuration_is_claimed :
    configurationAdmission true = .claim := rfl

theorem deferred_request_waits_within_its_generation (generation : Generation) :
    retriesDeferred generation generation = false := by
  simp [retriesDeferred]

theorem deferred_request_retries_under_newer_generation (generation : Generation) :
    retriesDeferred generation (generation + 1) = true := by
  simp [retriesDeferred]


def effectiveDispatchers (s : RuntimeState) : Finset AgentDid :=
  s.active.dispatchers \ s.startupDemoted

def effectiveUnavailable (s : RuntimeState) : Finset AgentDid :=
  s.active.unavailable ∪ s.startupDemoted

def bindSessionIfNeeded
    (s : RuntimeState)
    (sessionId : SessionId)
    (agentDid : AgentDid) :
    SessionId → Option AgentDid :=
  match s.sessionAgent sessionId with
  | some _ => s.sessionAgent
  | none => Function.update s.sessionAgent sessionId (some agentDid)

theorem bindSessionIfNeeded_requested
    (s : RuntimeState) (sessionId : SessionId) (requested : AgentDid)
    (h : (s.sessionAgent sessionId).all (· == requested) = true) :
    s.bindSessionIfNeeded sessionId requested sessionId = some requested := by
  unfold bindSessionIfNeeded
  cases hs : s.sessionAgent sessionId <;> simp_all

theorem bindSessionIfNeeded_other
    {s : RuntimeState}
    {sessionId other : SessionId}
    {agentDid : AgentDid}
    (h_other : other ≠ sessionId) :
    s.bindSessionIfNeeded sessionId agentDid other = s.sessionAgent other := by
  unfold bindSessionIfNeeded
  cases h : s.sessionAgent sessionId <;> simp [h, Function.update, h_other]

theorem bindSessionIfNeeded_eq_self_of_bound
    {s : RuntimeState}
    {sessionId : SessionId}
    {bound requested : AgentDid}
    (h_bound : s.sessionAgent sessionId = some bound) :
    s.bindSessionIfNeeded sessionId requested = s.sessionAgent := by
  funext sid
  unfold bindSessionIfNeeded
  simp [h_bound]

def AgentAdmissible
    (process : ProcessState)
    (s : RuntimeState)
    (agentDid : AgentDid) : Prop :=
  process.acceptsWork ∧
    0 < s.active.generation ∧
    s.routerObservedGeneration = s.active.generation ∧
    agentDid ∈ s.effectiveDispatchers ∧
    agentDid ∉ s.effectiveUnavailable

instance
    (process : ProcessState)
    (s : RuntimeState)
    (agentDid : AgentDid) :
    Decidable (AgentAdmissible process s agentDid) := by
  unfold AgentAdmissible
  infer_instance

def CanAdmitRequest
    (process : ProcessState)
    (s : RuntimeState)
    (sessionId : SessionId)
    (requestId : RequestId) (requested : AgentDid) : Prop :=
  requestId ∉ s.accepted ∧
    requestId ∉ s.inFlight ∧
    AgentAdmissible process s requested ∧
    (s.sessionAgent sessionId).all (· == requested) = true

instance
    (process : ProcessState)
    (s : RuntimeState)
    (sessionId : SessionId)
    (requestId : RequestId) (requested : AgentDid) :
    Decidable (CanAdmitRequest process s sessionId requestId requested) := by
  unfold CanAdmitRequest
  infer_instance

/-- Client-facing readiness is derived from the same runtime state and
effective dispatcher set used by request admission; configuration rows never
participate, and missing observations, a non-ready process, and generation
skew all fail closed. -/
inductive AgentUnavailableReason where
  | agentDisabled
  | runtimeConfigurationInvalid
  | backendNotConfigured
  | backendDisabled
  | backendTemporarilyUnavailable
  | credentialsRequired
  | subscriptionNotEntitled
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
  | .subscriptionNotEntitled => "subscription_not_entitled"
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
  | unknownStorageUnavailable
  deriving DecidableEq, Repr

namespace ClientAgentReadiness

def stateString : ClientAgentReadiness → String
  | .ready => "ready"
  | .unavailableRuntime _ | .unavailableStartup => "unavailable"
  | .unknownMissing | .unknownMalformed | .unknownVersion
  | .unknownProcess | .unknownStale | .unknownAbsent
  | .unknownStorageUnavailable => "unknown"

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
  | .unknownStorageUnavailable => some "storage_unavailable"

/-- External premise (Regolith `WriteLatch`, a `OnceCell` with no clear path):
once a write-ahead log append fails, every later write of this storage engine
fails with the retained error kind until the process reopens the store. The
durable readiness row is itself such a write, so the fault it reports cannot be
recorded in that row; `storageUnavailable` is a process-local observation the
reader joins with whatever the (now frozen) row still says. -/
inductive ClientNodeObservation where
  | missing
  | malformed
  | unsupportedVersion
  | storageUnavailable
  | observed (process : ProcessState) (state : RuntimeState)

/-- The reader joins the process-local storage fault with the durable row: a
fault dominates every row observation, including a row that still reads ready. -/
def ClientNodeObservation.joinStorageFault
    (storageUnavailable : Bool) (observation : ClientNodeObservation) :
    ClientNodeObservation :=
  if storageUnavailable then .storageUnavailable else observation

def project
    (observation : ClientNodeObservation)
    (agentDid : AgentDid)
    (runtimeUnavailableReason : AgentUnavailableReason) : ClientAgentReadiness :=
  match observation with
  | .missing => .unknownMissing
  | .malformed => .unknownMalformed
  | .unsupportedVersion => .unknownVersion
  | .storageUnavailable => .unknownStorageUnavailable
  | .observed process s =>
      if !decide process.acceptsWork then
        .unknownProcess
      else if s.active.generation = 0 ∨
        s.routerObservedGeneration ≠ s.active.generation then
        .unknownStale
      else if agentDid ∈ s.startupDemoted then
        .unavailableStartup
      else if agentDid ∈ s.active.unavailable then
        .unavailableRuntime runtimeUnavailableReason
      else if agentDid ∈ s.effectiveDispatchers then
        .ready
      else
        .unknownAbsent

/-- A storage fault never projects ready: the runtime that cannot write its
readiness row must not keep reporting readiness from the stale row. -/
theorem storage_unavailable_never_ready
    {agentDid : AgentDid}
    {reason : AgentUnavailableReason} :
    project .storageUnavailable agentDid reason ≠ .ready := by
  simp [project]

/-- The fault is not derivable from the durable row: joining it closes every
observation, including one whose row would otherwise project ready. -/
theorem joined_storage_fault_never_ready
    {observation : ClientNodeObservation}
    {agentDid : AgentDid}
    {reason : AgentUnavailableReason} :
    project (observation.joinStorageFault true) agentDid reason ≠ .ready := by
  simp [ClientNodeObservation.joinStorageFault, project]

/-- Without a process-local fault the reader projects the durable row unchanged. -/
theorem join_without_storage_fault
    {observation : ClientNodeObservation} :
    observation.joinStorageFault false = observation := by
  simp [ClientNodeObservation.joinStorageFault]

theorem ready_sound
    {process : ProcessState}
    {s : RuntimeState}
    {agentDid : AgentDid}
    (hReady : project (.observed process s) agentDid .backendTemporarilyUnavailable = .ready) :
    AgentAdmissible process s agentDid := by
  have hProcess : process.acceptsWork := by
    by_contra h
    simp [project, h] at hReady
  have hZero : s.active.generation ≠ 0 := by
    intro h
    simp [project, hProcess, h] at hReady
  have hGeneration : s.routerObservedGeneration = s.active.generation := by
    by_contra h
    simp [project, hProcess, hZero, h] at hReady
  have hDemoted : agentDid ∉ s.startupDemoted := by
    intro h
    simp [project, hProcess, hZero, hGeneration, h] at hReady
  have hActiveUnavailable : agentDid ∉ s.active.unavailable := by
    intro h
    simp [project, hProcess, hZero, hGeneration, hDemoted, h] at hReady
  have hDispatcher : agentDid ∈ s.effectiveDispatchers := by
    by_cases h : agentDid ∈ s.effectiveDispatchers
    · exact h
    · simp [project, hProcess, hZero, hGeneration, hDemoted, hActiveUnavailable, h] at hReady
  refine ⟨hProcess, Nat.pos_of_ne_zero hZero, hGeneration, hDispatcher, ?_⟩
  simp [effectiveUnavailable, hActiveUnavailable, hDemoted]

theorem missing_or_stale_never_ready
    {observation : ClientNodeObservation}
    {agentDid : AgentDid}
    (hClosed : observation = .missing ∨ observation = .malformed ∨
      observation = .unsupportedVersion ∨
      ∃ process s, observation = .observed process s ∧
        (s.active.generation = 0 ∨ s.routerObservedGeneration ≠ s.active.generation)) :
    project observation agentDid .backendTemporarilyUnavailable ≠ .ready := by
  rcases hClosed with hMissing | hMalformed | hVersion |
    ⟨process, s, hObservation, hStale⟩
  · simp [project, hMissing]
  · simp [project, hMalformed]
  · simp [project, hVersion]
  · subst observation
    rcases hStale with hZero | hGeneration
    · cases process <;> simp [project, ProcessState.acceptsWork, hZero]
    · cases process <;> simp [project, ProcessState.acceptsWork, hGeneration]

/-- A process that has just claimed the durable readiness row is never observed
as client-ready, whatever picture its predecessor left behind. This is what
makes a row written by a dead process unobservable without stamping process
identity on it: the claim rewrites the only field the projection checks first.
External premise: a runtime killed without shutdown leaves its last durable
`process_state` in place, the store publishes no liveness signal, and document
age is not a connectivity lease, so the successor's claim is the only fact that
distinguishes the two processes. -/
theorem claimed_process_is_never_client_ready
    {pre post : ProcessState}
    {s : RuntimeState}
    {agentDid : AgentDid}
    {reason : AgentUnavailableReason}
    (hClaim : ProcessState.step? pre .restartClaim = some post) :
    project (.observed post s) agentDid reason ≠ .ready := by
  cases pre <;> simp [ProcessState.step?] at hClaim <;> subst hClaim <;>
    simp [project, ProcessState.acceptsWork]

theorem unavailable_wins_overlap
    {process : ProcessState}
    {s : RuntimeState}
    {agentDid : AgentDid}
    (hUnavailable : agentDid ∈ s.effectiveUnavailable) :
    project (.observed process s) agentDid .backendTemporarilyUnavailable ≠ .ready := by
  intro hReady
  rcases ready_sound hReady with ⟨_, _, _, _, hNotUnavailable⟩
  exact hNotUnavailable hUnavailable

theorem observed_ready_iff_admissible
    {process : ProcessState}
    {s : RuntimeState}
    {agentDid : AgentDid} :
    project (.observed process s) agentDid .backendTemporarilyUnavailable = .ready ↔
      AgentAdmissible process s agentDid := by
  constructor
  · exact ready_sound
  · intro hAdmissible
    rcases hAdmissible with
      ⟨hProcess, hPositive, hGeneration, hDispatcher, hUnavailable⟩
    have hZero : s.active.generation ≠ 0 := Nat.ne_of_gt hPositive
    have hDemoted : agentDid ∉ s.startupDemoted := by
      intro h
      exact hUnavailable (by simp [effectiveUnavailable, h])
    have hActiveUnavailable : agentDid ∉ s.active.unavailable := by
      intro h
      exact hUnavailable (by simp [effectiveUnavailable, h])
    simp [project, hProcess, hZero, hGeneration, hDemoted, hActiveUnavailable, hDispatcher]

theorem ready_implies_runtime_admission_when_fresh
    {process : ProcessState}
    {s : RuntimeState}
    {sessionId : SessionId}
    {requestId : RequestId}
    {requested : AgentDid}
    (hBinding : (s.sessionAgent sessionId).all (· == requested) = true)
    (hReady : project (.observed process s) requested .backendTemporarilyUnavailable = .ready)
    (hUnaccepted : requestId ∉ s.accepted)
    (hNotInFlight : requestId ∉ s.inFlight) :
    CanAdmitRequest process s sessionId requestId requested := by
  exact ⟨hUnaccepted, hNotInFlight, ready_sound hReady, hBinding⟩

end ClientAgentReadiness

def coherent (s : RuntimeState) : Prop :=
  s.active.wellFormed ∧
    s.lastResolved.wellFormed ∧
    s.active.defaultAgent = s.lastResolved.defaultAgent ∧
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
      s.sessionAgent (s.requestSession rid) = some (s.requestAgent rid))

/-- `readyGenerations` is an internal transition guard, not an independent
client-wire admission fact. -/
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
  , requestAgent := fun _ => resolved.defaultAgent
  , sessionAgent := fun _ => none
  , generationSettings := fun _ _ => none
  , requestSettings := fun _ => none
  }

/-- No unaccepted request carries settings. This is what forbids a projection
from presenting an agent's configured selection as a pending or queued
request's effective settings: until the request is accepted the model has no
such fact to report. -/
def settingsOnlyWhenAccepted (s : RuntimeState) : Prop :=
  ∀ rid, rid ∉ s.accepted → s.requestSettings rid = none

theorem bootState_settingsOnlyWhenAccepted
    (resolved : ResolvedSnapshot) :
    (bootState resolved).settingsOnlyWhenAccepted := by
  intro _ _
  rfl

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
