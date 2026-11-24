import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.Runtime.RuntimeReconcile.StartupReadiness
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases SessionQueue

def readinessPublicationTrace (previous : Option Nat) : List Nat → List Bool
  | [] => []
  | next :: rest =>
      RuntimeReconcile.StartupReadiness.shouldPublish previous next ::
        readinessPublicationTrace (some next) rest

structure ReadinessPublicationInput where
  states : List Nat
  deriving Lean.ToJson

structure ReadinessPublicationOutput where
  publishes : List Bool
  deriving Lean.ToJson

@[conformance_family]
def readinessPublication : Family ReadinessPublicationInput ReadinessPublicationOutput where
  inputs := [ { states := [0, 0, 0, 1, 1, 2] }, { states := [0, 1, 0, 0, 2] } ]
  model := fun i => { publishes := readinessPublicationTrace none i.states }

structure ReconcileInput where
  name : String
  action : String
  requestedAgent : Option Nat
  preDefaultAgent : Nat
  preSessionAgent : Option Nat
  preRunnable : List Nat
  preStartupDemoted : List Nat
  prePhase : String
  preActiveGeneration : Nat
  preRouterGeneration : Nat
  preReadyGenerationCount : Nat
  preLiveGenerationCount : Nat
  preInFlightCount : Nat
  trackedRequestId : Nat
  trackedSessionId : Nat
  deriving Lean.ToJson

structure ReconcileSelectionOutput where
  legal : Bool
  deriving Lean.ToJson

structure ReconcileStatusOutput where
  postPhase : String
  postActiveGeneration : Nat
  postRouterGeneration : Nat
  deriving Lean.ToJson

structure ReconcileRouterOutput where
  legal : Bool
  postRouterGeneration : Nat
  deriving Lean.ToJson

structure ReconcileSupervisorOutput where
  postPhase : String
  postActiveGeneration : Nat
  deriving Lean.ToJson

private def reconcileCase (name : String) : RuntimeReconcileCase :=
  (runtimeReconcileCases.find? fun c => c.name == name).getD
    (runtimeCaseFromStep "" "applyFailed" runtimeBoot .applyFailed)

private def reconcileInput (c : RuntimeReconcileCase) : ReconcileInput :=
  { name := c.name, action := c.action, requestedAgent := c.requestedAgent
    preDefaultAgent := c.preDefaultAgent, preSessionAgent := c.preSessionAgent
    preRunnable := c.preRunnable, preStartupDemoted := c.preStartupDemoted
    prePhase := c.prePhase
    preActiveGeneration := c.preActiveGeneration, preRouterGeneration := c.preRouterGeneration
    preReadyGenerationCount := c.preReadyGenerationCount
    preLiveGenerationCount := c.preLiveGenerationCount, preInFlightCount := c.preInFlightCount
    trackedRequestId := c.trackedRequestId, trackedSessionId := c.trackedSessionId }

private def reconcileInputs (names : List String) : List ReconcileInput :=
  (runtimeReconcileCases.filter fun c => c.name ∈ names).map reconcileInput

/-- The router admits an explicit selection when the session's bound Agent
reads the session and is the requested one, and the routed generation holds a
ready dispatcher for it: the private-session fixture binds a session to one of
two readable Agents and the routed generation dispatches the requested Agent,
dispatches none for an unknown one, and holds a startup demotion for a demoted
one. -/
def selectionNames : List String :=
  ["default-A-explicit-B-binds-B", "existing-A-requested-B-rejected",
    "existing-A-requested-A-accepted", "unknown-selection-never-falls-back",
    "demoted-selection-never-falls-back"]

@[conformance_family]
def runtimeReconcileSelection : Family ReconcileInput ReconcileSelectionOutput where
  inputs := reconcileInputs selectionNames
  model := fun i => { legal := (reconcileCase i.name).legal }

/-- The runtime status handle persists a published generation and a router
observation, so it builds the publish and router-observe steps. -/
def statusNames : List String :=
  ["publish_changed_snapshot", "router_observe_published_generation"]

@[conformance_family]
def runtimeReconcileStatus : Family ReconcileInput ReconcileStatusOutput where
  inputs := reconcileInputs statusNames
  model := fun i =>
    let c := reconcileCase i.name
    { postPhase := c.postPhase, postActiveGeneration := c.postActiveGeneration
      postRouterGeneration := c.postRouterGeneration }

/-- The router observes the active snapshot's generation and holds a request
until that generation is the one it routes with, so it builds the router
observation, a request held while the published generation is unobserved, and
the accept after the observation. A held request leaves the routed generation
where it was. -/
def routerNames : List String :=
  ["router_observe_published_generation", "unobserved-generation-rejects-explicit-request",
    "accept_request_after_router_observe"]

@[conformance_family]
def runtimeReconcileRouter : Family ReconcileInput ReconcileRouterOutput where
  inputs := reconcileInputs routerNames
  model := fun i =>
    let c := reconcileCase i.name
    { legal := c.legal
      postRouterGeneration := if c.legal then c.postRouterGeneration else c.preRouterGeneration }

/-- The generation supervisor applies a proposed snapshot or keeps the
previous generation when the proposal fails to apply. -/
def supervisorNames : List String :=
  ["publish_changed_snapshot", "apply_failed_clears_pending"]

@[conformance_family]
def runtimeReconcileSupervisor : Family ReconcileInput ReconcileSupervisorOutput where
  inputs := reconcileInputs supervisorNames
  model := fun i =>
    let c := reconcileCase i.name
    { postPhase := c.postPhase, postActiveGeneration := c.postActiveGeneration }

example : (reconcileCase "publish_changed_snapshot").legal = true ∧
    (reconcileCase "apply_failed_clears_pending").legal = true := by native_decide

/-- A resolved snapshot becomes the runtime's generation only when every
runnable Agent's dependencies are present: the generation supervisor refuses a
proposal whose runnable Agent has no tool surface and keeps the previous
generation. -/
def resolveNames : List String := ["missing_dependency_snapshot_is_not_resolved"]

structure ReconcileResolveOutput where
  legal : Bool
  deriving Lean.ToJson

@[conformance_family]
def runtimeReconcileResolve : Family ReconcileInput ReconcileResolveOutput where
  inputs := reconcileInputs resolveNames
  model := fun i => { legal := (reconcileCase i.name).legal }

/-- A retired generation ends once no call it dispatched is in flight.
Production tracks in-flight work per Agent slot: a published generation that
replaces an Agent's slot retires the old slot, which keeps running the call in
flight and exits when that call finishes, so a slot's generation is the
generation of every call it runs. A slot's request channel is held open only
by the generation snapshots that name it (the supervisor's, the published
watch's and the router's routed one); once the next generation is published
and routed, none names the retired slot, so its channel closes and its task
exits when its last call finishes. The runtime fixture restages the Agent's
slot with no call, with a claimed request whose inference call the model
endpoint holds, and after the endpoint answers it, and counts the slot tasks
not yet joined. -/
def retireNames : List String :=
  ["retire_unobserved_generation", "retired_generation_waits_for_its_call_in_flight",
    "retire_after_its_call_finishes"]

structure ReconcileRetireOutput where
  legal : Bool
  postLiveGenerationCount : Nat
  deriving Lean.ToJson

@[conformance_family]
def runtimeReconcileRetire : Family ReconcileInput ReconcileRetireOutput where
  inputs := reconcileInputs retireNames
  model := fun i =>
    let c := reconcileCase i.name
    { legal := c.legal
      postLiveGenerationCount :=
        if c.legal then c.postLiveGenerationCount else c.preLiveGenerationCount }

/-- A finished call leaves the in-flight set, which finalizes its request's
claim and releases the backend slot its inference call held: the runtime
fixture answers the call held on a retired slot and counts the request as in
flight while it is claimed or processing or an inference call of it holds a
backend slot. -/
def finishNames : List String := ["finish_request_releases_generation"]

structure ReconcileFinishOutput where
  legal : Bool
  postInFlightCount : Nat
  deriving Lean.ToJson

@[conformance_family]
def runtimeReconcileFinish : Family ReconcileInput ReconcileFinishOutput where
  inputs := reconcileInputs finishNames
  model := fun i =>
    let c := reconcileCase i.name
    { legal := c.legal, postInFlightCount := c.postInFlightCount }

/-- Every reconcile case belongs to the family of the production owner that
decides it. -/
example : runtimeReconcileCases.all (fun c =>
    (selectionNames ++ statusNames ++ routerNames ++ supervisorNames ++ resolveNames ++
      retireNames ++ finishNames).contains c.name) = true := by native_decide

/-- Generation 1 retires when idle and after its call finishes, and not while
the call is in flight. -/
example : (retireNames.map fun n => (reconcileCase n).legal) = [true, false, true] := by
  native_decide

structure QueueAdmissionInput where
  name : String
  sessionId : Nat
  queueKey : Option String
  preActiveRequestId : Option Nat
  prePendingRequestIds : List Nat
  deriving Lean.ToJson

structure QueueAdmissionOutput where
  legal : Bool
  postActiveRequestId : Option Nat
  postPendingRequestIds : List Nat
  postTerminalRequestIds : List Nat
  postCoalescedPendingCount : Nat
  claimedRequestId : Option Nat
  blockedByActive : Bool
  automatedDrainedRequestIds : List Nat
  preservedUserPendingRequestIds : List Nat
  deriving Lean.ToJson

def queueDeadlineCase? (name : String) : Option QueueDeadlineConformanceCase :=
  queueDeadlineConformanceCases.find? (·.name == name)

/-- The request-row runtime drives the queue claims and the cancellation
drain; the background completion notification creates no request row, and
the explicit-deadline claim is observed on its own row. -/
@[conformance_family]
def queueDeadlineAdmission : Family QueueAdmissionInput QueueAdmissionOutput where
  inputs := (queueDeadlineConformanceCases.filter fun c =>
      c.name ∈ ["active_request_blocks_later_same_session_claim",
        "terminal_active_allows_next_pending_same_session_claim",
        "cancel_drains_automated_wakeups_preserves_user_pending"]).map fun c =>
    { name := c.name, sessionId := c.sessionId, queueKey := c.queueKey
      preActiveRequestId := c.preActiveRequestId, prePendingRequestIds := c.prePendingRequestIds }
  model := fun i => match queueDeadlineCase? i.name with
    | some c =>
        { legal := c.legal, postActiveRequestId := c.postActiveRequestId
          postPendingRequestIds := c.postPendingRequestIds
          postTerminalRequestIds := c.postTerminalRequestIds
          postCoalescedPendingCount := c.postCoalescedPendingCount
          claimedRequestId := c.claimedRequestId, blockedByActive := c.blockedByActive
          automatedDrainedRequestIds := c.automatedDrainedRequestIds
          preservedUserPendingRequestIds := c.preservedUserPendingRequestIds }
    | none =>
        { legal := false, postActiveRequestId := none, postPendingRequestIds := []
          postTerminalRequestIds := [], postCoalescedPendingCount := 0, claimedRequestId := none
          blockedByActive := false, automatedDrainedRequestIds := []
          preservedUserPendingRequestIds := [] }

structure ClaimDeadlineInput where
  name : String
  sessionId : Nat
  claimedRequestId : Option Nat
  preRequestDeadline : Option Nat
  synthesizedClaimDeadline : Option Nat
  deriving Lean.ToJson

structure ClaimDeadlineOutput where
  legal : Bool
  explicitDeadlinePreserved : Bool
  deriving Lean.ToJson

@[conformance_family]
def queueDeadlineClaimDeadline : Family ClaimDeadlineInput ClaimDeadlineOutput where
  inputs := (queueDeadlineConformanceCases.filter (·.group == "claim_deadline")).map fun c =>
    { name := c.name, sessionId := c.sessionId, claimedRequestId := c.claimedRequestId
      preRequestDeadline := c.preRequestDeadline
      synthesizedClaimDeadline := c.synthesizedClaimDeadline }
  model := fun i => match queueDeadlineCase? i.name with
    | some c => { legal := c.legal, explicitDeadlinePreserved := c.explicitDeadlinePreserved }
    | none => { legal := false, explicitDeadlinePreserved := false }

structure CompletionDeliveryInput where
  name : String
  sessionId : Nat
  queueKey : Option String
  prePendingRequestIds : List Nat
  deriving Lean.ToJson

structure CompletionDeliveryOutput where
  postCoalescedPendingCount : Nat
  deriving Lean.ToJson

@[conformance_family]
def queueDeadlineCompletionDelivery :
    Family CompletionDeliveryInput CompletionDeliveryOutput where
  inputs := (queueDeadlineConformanceCases.filter (·.group == "completion_delivery")).map fun c =>
    { name := c.name, sessionId := c.sessionId, queueKey := c.queueKey
      prePendingRequestIds := c.prePendingRequestIds }
  model := fun i =>
    { postCoalescedPendingCount :=
        ((queueDeadlineCase? i.name).map (·.postCoalescedPendingCount)).getD 0 }

example : backgroundCompletionCreatesNoAgentRequestCase.group = "completion_delivery" ∧
    backgroundCompletionCreatesNoAgentRequestCase.action = "appendNotification" ∧
    backgroundCompletionCreatesNoAgentRequestCase.legal = true ∧
    backgroundCompletionCreatesNoAgentRequestCase.preActiveRequestId =
      backgroundCompletionCreatesNoAgentRequestCase.postActiveRequestId ∧
    backgroundCompletionCreatesNoAgentRequestCase.postPendingRequestIds = [] := by
  native_decide

structure InterruptScopeInput where
  sessionId : Nat
  queueKey : Option String
  automatedRequestIds : List Nat
  userRequestIds : List Nat
  foreignRequesterRequestIds : List Nat
  foreignOwnerRequestIds : List Nat
  deriving Lean.ToJson

structure InterruptScopeOutput where
  automatedDrainedRequestIds : List Nat
  preservedUserPendingRequestIds : List Nat
  preservedForeignRequesterRequestIds : List Nat
  preservedForeignOwnerRequestIds : List Nat
  deriving Lean.ToJson

def interruptScope (i : InterruptScopeInput) : InterruptScopeOutput :=
  let key := i.sessionId
  let own := queueState i.sessionId none
    ((i.automatedRequestIds.map fun id => backgroundCompletionEntry id 10 key) ++
      i.userRequestIds.map fun id => userEntry id 11 none)
  let drain := fun (s : SessionQueueState) =>
    (SessionQueue.scopedStep? own.scope s (.drainAutomated .backgroundCompletion (some key))).getD s
  let post := drain own
  let requesterPending := i.foreignRequesterRequestIds.map fun id =>
    backgroundCompletionEntry id 12 key
  let ownerPending := i.foreignOwnerRequestIds.map fun id => backgroundCompletionEntry id 13 key
  let requester := drain (SessionQueueState.mk ⟨1, i.sessionId, some 2⟩ own.active
    requesterPending own.terminal own.folding)
  let owner := drain (SessionQueueState.mk ⟨2, i.sessionId, none⟩ own.active
    ownerPending own.terminal own.folding)
  { automatedDrainedRequestIds := terminalIds (i.automatedRequestIds ++ i.userRequestIds) post
    preservedUserPendingRequestIds := i.userRequestIds.filter fun id =>
      if id ∈ requestIds post.pending then true else false
    preservedForeignRequesterRequestIds := requestIds requester.pending
    preservedForeignOwnerRequestIds := requestIds owner.pending }

def interruptScopeInput : InterruptScopeInput :=
  { sessionId := 900, queueKey := some (queueKeyLabel QueueSource.backgroundCompletion 900)
    automatedRequestIds := [301], userRequestIds := [302]
    foreignRequesterRequestIds := [303], foreignOwnerRequestIds := [304] }

/-- The interrupt latch drains the automated wake-up of its own owner and
requester scope; the user's pending row and the same-session rows of a
foreign requester or owner stay pending. -/
@[conformance_family]
def queueDeadlineInterruptScope : Family InterruptScopeInput InterruptScopeOutput where
  inputs := [interruptScopeInput]
  model := interruptScope

example :
    (interruptScope interruptScopeInput).automatedDrainedRequestIds =
      cancelDrainsAutomatedPreservesUserCase.automatedDrainedRequestIds ∧
    (interruptScope interruptScopeInput).preservedUserPendingRequestIds =
      cancelDrainsAutomatedPreservesUserCase.preservedUserPendingRequestIds ∧
    (interruptScope interruptScopeInput).preservedForeignRequesterRequestIds =
      cancelDrainsAutomatedPreservesUserCase.preservedForeignRequesterRequestIds ∧
    (interruptScope interruptScopeInput).preservedForeignOwnerRequestIds =
      cancelDrainsAutomatedPreservesUserCase.preservedForeignOwnerRequestIds := by
  native_decide

deriving instance Lean.ToJson for RuntimeReconcile.StartupReadiness.BuildOutcome

structure StartupReadinessInput where
  witness : String
  budget : Nat
  outcomes : List RuntimeReconcile.StartupReadiness.BuildOutcome
  retiredAfter : Bool
  deriving Lean.ToJson

structure StartupReadinessOutput where
  postStanding : String
  blocksReady : Bool
  deriving Lean.ToJson

def startupStandingName : RuntimeReconcile.StartupReadiness.AgentStanding → String
  | .pending _ => "pending"
  | .ready => "ready"
  | .demoted => "demoted"
  | .superseded => "superseded"

/-- Each case releases from its build outcomes and retirement alone: no case
restarts the runtime to leave `pending`. -/
@[conformance_family]
def startupReadiness : Family StartupReadinessInput StartupReadinessOutput where
  inputs :=
    [ { witness := "startup_readiness.persistent_build_failure_demotes"
        budget := 3, outcomes := [.failed, .failed, .failed], retiredAfter := false }
    , { witness := "startup_readiness.transient_failure_then_start_is_ready"
        budget := 3, outcomes := [.failed, .started], retiredAfter := false }
    , { witness := "startup_readiness.demotion_is_not_readiness"
        budget := 1, outcomes := [.failed], retiredAfter := false }
    , { witness := "startup_readiness.within_budget_still_pending"
        budget := 3, outcomes := [.failed], retiredAfter := false }
    , { witness := "startup_readiness.ready_is_absorbing"
        budget := 3, outcomes := [.started, .failed, .failed, .failed, .failed]
        retiredAfter := false }
    , { witness := "startup_readiness.retirement_releases_a_pending_agent"
        budget := 3, outcomes := [.failed], retiredAfter := true } ]
  model := fun i =>
    let built := RuntimeReconcile.StartupReadiness.run i.budget
      RuntimeReconcile.StartupReadiness.seeded i.outcomes
    let standing := if i.retiredAfter then RuntimeReconcile.StartupReadiness.retire built else built
    { postStanding := startupStandingName standing
      blocksReady := !standing.released }

structure SnapshotDemotionInput where
  name : String
  defaultAgent : Nat
  runnable : List Nat
  unavailable : List Nat
  demotions : List Nat
  deriving Lean.ToJson

structure SnapshotDemotionOutput where
  postRunnable : List Nat
  postUnavailable : List Nat
  postDefaultAgent : Nat
  wellFormed : Bool
  deriving Lean.ToJson

def snapshotDemotionInput (name : String) (unavailable : List Nat) (demotions : List Nat) :
    SnapshotDemotionInput :=
  { name, defaultAgent := 1, runnable := [1, 2], unavailable, demotions }

@[conformance_family]
def snapshotDemotion : Family SnapshotDemotionInput SnapshotDemotionOutput where
  inputs :=
    [ snapshotDemotionInput "snapshot_demotion.no_failures" [] []
    , snapshotDemotionInput "snapshot_demotion.other_agent" [] [2]
    , snapshotDemotionInput "snapshot_demotion.default_agent" [] [1]
    , snapshotDemotionInput "snapshot_demotion.all_agents" [] [1, 2]
    , snapshotDemotionInput "snapshot_demotion.repeated_failure" [] [2, 2]
    , snapshotDemotionInput "snapshot_demotion.retains_prior_unavailability" [3] [2] ]
  model := fun i =>
    let pre : ResolvedSnapshot :=
      { defaultAgent := i.defaultAgent, runnable := i.runnable.toFinset,
        unavailable := i.unavailable.toFinset, dependenciesSatisfied := i.runnable.toFinset }
    let post := i.demotions.foldl ResolvedSnapshot.demote pre
    { postRunnable := post.runnable.sort (· ≤ ·)
      postUnavailable := post.unavailable.sort (· ≤ ·)
      postDefaultAgent := post.defaultAgent
      wellFormed := decide post.wellFormed }

end Conformance.Contracts
