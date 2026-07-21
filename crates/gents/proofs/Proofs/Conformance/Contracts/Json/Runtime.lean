import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases

/-!
# Runtime and Recovery JSON

Serializers for runtime reconcile, session recovery, and queue deadline
contract rows.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def runtimeReconcileCaseJson (witness : RuntimeReconcileCase) : String :=
  "{"
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
    ++ "\"tracked_request_behavior\":" ++ toString witness.trackedRequestBehavior ++ ","
    ++ "\"tracked_session_behavior\":" ++ toString witness.trackedSessionBehavior
    ++ "}"

def sessionRecoveryCaseJson (witness : SessionRecoveryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_latest_state\":" ++ jsonString witness.preLatestState ++ ","
    ++ "\"pre_failed_state\":" ++ jsonString witness.preFailedState ++ ","
    ++ "\"post_latest_state\":" ++ jsonString witness.postLatestState ++ ","
    ++ "\"post_failed_state\":" ++ jsonString witness.postFailedState ++ ","
    ++ "\"post_new_state\":" ++ jsonString witness.postNewState ++ ","
    ++ "\"pre_latest_admission\":" ++ jsonString witness.preLatestAdmission ++ ","
    ++ "\"post_latest_admission\":" ++ jsonString witness.postLatestAdmission ++ ","
    ++ "\"pre_failed_admission\":" ++ jsonString witness.preFailedAdmission ++ ","
    ++ "\"post_failed_admission\":" ++ jsonString witness.postFailedAdmission ++ ","
    ++ "\"post_new_admission\":" ++ jsonString witness.postNewAdmission ++ ","
    ++ "\"pre_origin\":" ++ jsonString witness.preOrigin ++ ","
    ++ "\"post_new_origin\":" ++ jsonString witness.postNewOrigin ++ ","
    ++ "\"pre_backend\":" ++ jsonString witness.preBackend ++ ","
    ++ "\"post_new_backend\":" ++ jsonString witness.postNewBackend ++ ","
    ++ "\"failed_id\":" ++ toString witness.failedId ++ ","
    ++ "\"new_id\":" ++ toString witness.newId ++ ","
    ++ "\"pre_latest_id\":" ++ toString witness.preLatestId ++ ","
    ++ "\"post_latest_id\":" ++ toString witness.postLatestId ++ ","
    ++ "\"pre_session_id\":" ++ toString witness.preSessionId ++ ","
    ++ "\"post_session_id\":" ++ toString witness.postSessionId ++ ","
    ++ "\"pre_behavior_id\":" ++ toString witness.preBehaviorId ++ ","
    ++ "\"post_behavior_id\":" ++ toString witness.postBehaviorId ++ ","
    ++ "\"pre_request_count\":" ++ toString witness.preRequestCount ++ ","
    ++ "\"post_request_count\":" ++ toString witness.postRequestCount ++ ","
    ++ "\"pre_retry_count\":" ++ toString witness.preRetryCount ++ ","
    ++ "\"post_retry_count\":" ++ toString witness.postRetryCount ++ ","
    ++ "\"max_retries\":" ++ toString witness.maxRetries ++ ","
    ++ "\"pre_deadline_exceeded\":" ++ boolString witness.preDeadlineExceeded ++ ","
    ++ "\"post_deadline_exceeded\":" ++ boolString witness.postDeadlineExceeded ++ ","
    ++ "\"pre_failed_is_latest\":" ++ boolString witness.preFailedIsLatest ++ ","
    ++ "\"post_failed_is_latest\":" ++ boolString witness.postFailedIsLatest ++ ","
    ++ "\"post_new_is_latest\":" ++ boolString witness.postNewIsLatest ++ ","
    ++ "\"pre_request_ids\":" ++ jsonArray (witness.preRequestIds.map toString) ++ ","
    ++ "\"pre_failed_exists\":" ++ boolString witness.preFailedExists ++ ","
    ++ "\"pre_latest_exists\":" ++ boolString witness.preLatestExists ++ ","
    ++ "\"pre_new_request_exists\":" ++ boolString witness.preNewRequestExists ++ ","
    ++ "\"old_request_retained\":" ++ boolString witness.oldRequestRetained ++ ","
    ++ "\"new_request_inserted\":" ++ boolString witness.newRequestInserted ++ ","
    ++ "\"origin_preserved\":" ++ boolString witness.originPreserved ++ ","
    ++ "\"backend_preserved\":" ++ boolString witness.backendPreserved
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


/-- Startup-readiness vectors for the bounded build-failure barrier
(gents#559).

`outcomes` is the build-attempt sequence the slot observes; `post_standing` is
the behavior's standing with the barrier afterwards. `blocks_ready` pins the
liveness claim: a released standing never holds `Ready` hostage, and the only
standing that may is `pending`. `requires_restart` is pinned `false` everywhere
— release follows from the budget, never from restarting the process. -/
structure StartupReadinessCase where
  witness : String
  leanTheorems : List String
  budget : Nat
  outcomes : List String
  /-- Reconcile retires the slot after the outcomes are observed. -/
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
  [ -- #559 itself: every build fails; the budget demotes instead of wedging.
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
    -- A transient failure within the budget still reaches ready.
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
    -- Demotion never claims health: a spent budget is not a start.
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
    -- Under budget with no success yet: still pending, still blocking Ready.
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
    -- Ready is absorbing: post-start outcomes never re-enter the barrier.
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
    -- Reconcile retires a never-started slot mid-startup: released without a
    -- health claim, instead of orphaning the pending entry (the second #559
    -- hang path).
  , { witness := "startup_readiness.retirement_releases_a_pending_behavior"
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

end Conformance.Contracts
