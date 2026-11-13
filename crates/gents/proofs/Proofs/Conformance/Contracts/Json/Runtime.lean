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

def runtimeReconcileCaseJson (witness : RuntimeReconcileCase) : String :=
  "{"
    ++ "\"requested_agent\":" ++ jsonOptionalNat witness.requestedAgent ++ ","
    ++ "\"pre_default_agent\":" ++ toString witness.preDefaultAgent ++ ","
    ++ "\"pre_session_agent\":" ++ jsonOptionalNat witness.preSessionAgent ++ ","
    ++ "\"pre_runnable\":" ++ jsonArray (witness.preRunnable.map toString) ++ ","
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_phase\":" ++ jsonString witness.prePhase ++ ","
    ++ "\"post_phase\":" ++ jsonString witness.postPhase ++ ","
    ++ "\"pre_active_generation\":" ++ toString witness.preActiveGeneration ++ ","
    ++ "\"post_active_generation\":" ++ toString witness.postActiveGeneration ++ ","
    ++ "\"pre_router_generation\":" ++ toString witness.preRouterGeneration ++ ","
    ++ "\"post_router_generation\":" ++ toString witness.postRouterGeneration ++ ","
    ++ "\"pre_ready_generation_count\":" ++ toString witness.preReadyGenerationCount ++ ","
    ++ "\"post_ready_generation_count\":" ++ toString witness.postReadyGenerationCount ++ ","
    ++ "\"pre_live_generation_count\":" ++ toString witness.preLiveGenerationCount ++ ","
    ++ "\"post_live_generation_count\":" ++ toString witness.postLiveGenerationCount ++ ","
    ++ "\"pre_in_flight_count\":" ++ toString witness.preInFlightCount ++ ","
    ++ "\"post_in_flight_count\":" ++ toString witness.postInFlightCount ++ ","
    ++ "\"tracked_request_id\":" ++ toString witness.trackedRequestId ++ ","
    ++ "\"tracked_session_id\":" ++ toString witness.trackedSessionId ++ ","
    ++ "\"tracked_request_generation\":" ++ toString witness.trackedRequestGeneration ++ ","
    ++ "\"tracked_request_session\":" ++ toString witness.trackedRequestSession ++ ","
    ++ "\"tracked_request_agent\":" ++ toString witness.trackedRequestAgent ++ ","
    ++ "\"tracked_session_agent\":" ++ toString witness.trackedSessionAgent
    ++ "}"

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
