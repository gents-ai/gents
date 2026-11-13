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
