import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.Runtime.RuntimeReconcile.StartupReadiness

namespace Conformance.Contracts

open Conformance.ContractCases

def readinessPublicationTrace (previous : Option Nat) : List Nat → List Bool
  | [] => []
  | next :: rest =>
      RuntimeReconcile.StartupReadiness.shouldPublish previous next ::
        readinessPublicationTrace (some next) rest

def readinessPublicationCasesJson : String :=
  jsonArray ([ [0, 0, 0, 1, 1, 2], [0, 1, 0, 0, 2] ].map fun states =>
    "{\"states\":" ++ jsonArray (states.map toString) ++
    ",\"publishes\":" ++ jsonArray ((readinessPublicationTrace none states).map boolString) ++ "}")

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

structure StartupReadinessCase where
  witness : String
  leanTheorems : List String
  budget : Nat
  outcomes : List String
  retiredAfter : Bool
  postStanding : String
  blocksReady : Bool
  requiresRestart : Bool

def startupReadinessCaseJson (witness : StartupReadinessCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"budget\":" ++ toString witness.budget ++ ","
    ++ "\"outcomes\":" ++ jsonStringArray witness.outcomes ++ ","
    ++ "\"retired_after\":" ++ boolString witness.retiredAfter ++ ","
    ++ "\"post_standing\":" ++ jsonString witness.postStanding ++ ","
    ++ "\"blocks_ready\":" ++ boolString witness.blocksReady ++ ","
    ++ "\"requires_restart\":" ++ boolString witness.requiresRestart
    ++ "}"

def startupReadinessCases : List StartupReadinessCase :=
  [
    { witness := "startup_readiness.persistent_build_failure_demotes"
    , leanTheorems :=
        [ "RuntimeReconcile.StartupReadiness.budgeted_attempts_release"
        , "RuntimeReconcile.StartupReadiness.seeded_release"
        , "RuntimeReconcile.StartupReadiness.demoted_consumed_the_budget"
        ]
    , budget := 3
    , outcomes := ["failed", "failed", "failed"]
    , retiredAfter := false
    , postStanding := "demoted"
    , blocksReady := false
    , requiresRestart := false
    }
  , { witness := "startup_readiness.transient_failure_then_start_is_ready"
    , leanTheorems :=
        [ "RuntimeReconcile.StartupReadiness.start_within_budget_is_ready"
        , "RuntimeReconcile.StartupReadiness.ready_requires_a_start"
        ]
    , budget := 3
    , outcomes := ["failed", "started"]
    , retiredAfter := false
    , postStanding := "ready"
    , blocksReady := false
    , requiresRestart := false
    }
  , { witness := "startup_readiness.demotion_is_not_readiness"
    , leanTheorems :=
        [ "RuntimeReconcile.StartupReadiness.ready_requires_a_start"
        ]
    , budget := 1
    , outcomes := ["failed"]
    , retiredAfter := false
    , postStanding := "demoted"
    , blocksReady := false
    , requiresRestart := false
    }
  , { witness := "startup_readiness.within_budget_still_pending"
    , leanTheorems :=
        [ "RuntimeReconcile.StartupReadiness.budgeted_attempts_release"
        ]
    , budget := 3
    , outcomes := ["failed"]
    , retiredAfter := false
    , postStanding := "pending"
    , blocksReady := true
    , requiresRestart := false
    }
  , { witness := "startup_readiness.ready_is_absorbing"
    , leanTheorems :=
        [ "RuntimeReconcile.StartupReadiness.released_absorbing"
        ]
    , budget := 3
    , outcomes := ["started", "failed", "failed", "failed", "failed"]
    , retiredAfter := false
    , postStanding := "ready"
    , blocksReady := false
    , requiresRestart := false
    }
  , { witness := "startup_readiness.retirement_releases_a_pending_agent"
    , leanTheorems :=
        [ "RuntimeReconcile.StartupReadiness.retire_releases"
        , "RuntimeReconcile.StartupReadiness.retire_never_claims_ready"
        ]
    , budget := 3
    , outcomes := ["failed"]
    , retiredAfter := true
    , postStanding := "superseded"
    , blocksReady := false
    , requiresRestart := false
    }
  ]

def startupReadinessCasesJson : String :=
  jsonArray (startupReadinessCases.map startupReadinessCaseJson)

def snapshotDemotionCaseJson (name : String) (unavailable : Finset AgentDid)
    (demotions : List AgentDid) : String :=
  let pre : ResolvedSnapshot :=
    { defaultAgent := 1, runnable := {1, 2}, unavailable := unavailable,
      dependenciesSatisfied := {1, 2} }
  let post := demotions.foldl ResolvedSnapshot.demote pre
  let ids := fun (values : Finset AgentDid) => jsonArray ((values.sort (· ≤ ·)).map toString)
  "{\"witness\":" ++ jsonString name ++
    ",\"default_agent\":" ++ toString pre.defaultAgent ++
    ",\"runnable\":" ++ ids pre.runnable ++
    ",\"unavailable\":" ++ ids pre.unavailable ++
    ",\"demotions\":" ++ jsonArray (demotions.map toString) ++
    ",\"post_runnable\":" ++ ids post.runnable ++
    ",\"post_unavailable\":" ++ ids post.unavailable ++
    ",\"post_default_agent\":" ++ toString post.defaultAgent ++
    ",\"well_formed\":" ++ boolString (decide post.wellFormed) ++ "}"

def snapshotDemotionCasesJson : String :=
  jsonArray [
    snapshotDemotionCaseJson "snapshot_demotion.no_failures" {} [],
    snapshotDemotionCaseJson "snapshot_demotion.other_agent" {} [2],
    snapshotDemotionCaseJson "snapshot_demotion.default_agent" {} [1],
    snapshotDemotionCaseJson "snapshot_demotion.all_agents" {} [1, 2],
    snapshotDemotionCaseJson "snapshot_demotion.repeated_failure" {} [2, 2],
    snapshotDemotionCaseJson "snapshot_demotion.retains_prior_unavailability" {3} [2]
  ]

end Conformance.Contracts
