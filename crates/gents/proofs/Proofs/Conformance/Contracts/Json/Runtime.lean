import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.Runtime.RuntimeReconcile.StartupReadiness
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

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
  prePhase : String
  preActiveGeneration : Nat
  preRouterGeneration : Nat
  preReadyGenerationCount : Nat
  preLiveGenerationCount : Nat
  preInFlightCount : Nat
  trackedRequestId : Nat
  trackedSessionId : Nat
  deriving Lean.ToJson

structure ReconcileOutput where
  legal : Bool
  postPhase : String
  postActiveGeneration : Nat
  postRouterGeneration : Nat
  postReadyGenerationCount : Nat
  postLiveGenerationCount : Nat
  postInFlightCount : Nat
  trackedRequestGeneration : Nat
  trackedRequestSession : Nat
  trackedRequestAgent : Nat
  trackedSessionAgent : Nat
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
    preRunnable := c.preRunnable, prePhase := c.prePhase
    preActiveGeneration := c.preActiveGeneration, preRouterGeneration := c.preRouterGeneration
    preReadyGenerationCount := c.preReadyGenerationCount
    preLiveGenerationCount := c.preLiveGenerationCount, preInFlightCount := c.preInFlightCount
    trackedRequestId := c.trackedRequestId, trackedSessionId := c.trackedSessionId }

private def reconcileInputs (names : List String) : List ReconcileInput :=
  (runtimeReconcileCases.filter fun c => c.name ∈ names).map reconcileInput

/-- Agent resolution reads the session's bound Agent and the requested Agent;
the private-session fixture binds a session to one of two readable Agents, so
it builds the explicit selections between two runnable Agents on a ready
runtime. -/
def selectionNames : List String :=
  ["default-A-explicit-B-binds-B", "existing-A-requested-B-rejected",
    "existing-A-requested-A-accepted"]

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
observation and the accept after it. -/
def routerNames : List String :=
  ["router_observe_published_generation", "accept_request_after_router_observe"]

@[conformance_family]
def runtimeReconcileRouter : Family ReconcileInput ReconcileRouterOutput where
  inputs := reconcileInputs routerNames
  model := fun i =>
    let c := reconcileCase i.name
    { legal := c.legal, postRouterGeneration := c.postRouterGeneration }

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

/-- model-production-gap: no production scenario drives a request finish
releasing its generation, a replayed accept, retiring an unobserved
generation, a resolve over a snapshot with a missing dependency, an unknown or
demoted explicit selection, or an explicit request before the router observes
the published generation; the served runtime fixtures cannot seed these
pre-states. -/
@[conformance_family]
def runtimeReconcileUnreached : Family ReconcileInput ReconcileOutput where
  inputs := (runtimeReconcileCases.filter fun c =>
    !(selectionNames ++ statusNames ++ routerNames ++ supervisorNames).contains c.name).map
      reconcileInput
  model := fun i =>
    let c := reconcileCase i.name
    { legal := c.legal, postPhase := c.postPhase
      postActiveGeneration := c.postActiveGeneration
      postRouterGeneration := c.postRouterGeneration
      postReadyGenerationCount := c.postReadyGenerationCount
      postLiveGenerationCount := c.postLiveGenerationCount
      postInFlightCount := c.postInFlightCount
      trackedRequestGeneration := c.trackedRequestGeneration
      trackedRequestSession := c.trackedRequestSession
      trackedRequestAgent := c.trackedRequestAgent
      trackedSessionAgent := c.trackedSessionAgent }

def queueDeadlineConformanceCaseJson
    (witness : QueueDeadlineConformanceCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"session_id\":" ++ toString witness.sessionId ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_active_request_id\":"
      ++ jsonOptionalNat witness.preActiveRequestId ++ ","
    ++ "\"post_active_request_id\":"
      ++ jsonOptionalNat witness.postActiveRequestId ++ ","
    ++ "\"pre_pending_request_ids\":"
      ++ jsonArray (witness.prePendingRequestIds.map toString) ++ ","
    ++ "\"post_pending_request_ids\":"
      ++ jsonArray (witness.postPendingRequestIds.map toString) ++ ","
    ++ "\"claimed_request_id\":"
      ++ jsonOptionalNat witness.claimedRequestId ++ ","
    ++ "\"blocked_by_active\":" ++ boolString witness.blockedByActive ++ ","
    ++ "\"superseded_request_ids\":"
      ++ jsonArray (witness.supersededRequestIds.map toString) ++ ","
    ++ "\"queue_key\":" ++ jsonOptionalString witness.queueKey ++ ","
    ++ "\"post_coalesced_pending_count\":"
      ++ toString witness.postCoalescedPendingCount ++ ","
    ++ "\"automated_drained_request_ids\":"
      ++ jsonArray (witness.automatedDrainedRequestIds.map toString) ++ ","
    ++ "\"preserved_foreign_requester_request_ids\":"
    ++ jsonArray (witness.preservedForeignRequesterRequestIds.map toString) ++ ","
    ++ "\"preserved_foreign_owner_request_ids\":"
    ++ jsonArray (witness.preservedForeignOwnerRequestIds.map toString) ++ ","
    ++ "\"preserved_user_pending_request_ids\":"
      ++ jsonArray (witness.preservedUserPendingRequestIds.map toString) ++ ","
    ++ "\"post_terminal_request_ids\":"
      ++ jsonArray (witness.postTerminalRequestIds.map toString) ++ ","
    ++ "\"pre_request_deadline\":"
      ++ jsonOptionalNat witness.preRequestDeadline ++ ","
    ++ "\"synthesized_claim_deadline\":"
      ++ jsonOptionalNat witness.synthesizedClaimDeadline ++ ","
    ++ "\"post_deadline\":" ++ jsonOptionalNat witness.postDeadline ++ ","
    ++ "\"explicit_deadline_preserved\":"
      ++ boolString witness.explicitDeadlinePreserved
    ++ "}"

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
