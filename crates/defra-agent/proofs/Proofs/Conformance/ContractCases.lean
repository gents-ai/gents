import Proofs.Fleet
import Proofs.InferenceCall.SlotAccounting
import Proofs.RuntimeReconcile
import Proofs.SessionRecovery

/-!
# Finite Conformance Witness Cases

Representative executable witnesses emitted by `Proofs.Conformance.Contracts`.
The cases stay finite and deterministic so Rust can consume them as a contract
without re-implementing Lean evaluation.
-/

namespace Conformance.ContractCases

structure RuntimeReconcileCase where
  name : String
  action : String
  legal : Bool
  prePhase : String
  postPhase : String
  preActiveGeneration : Nat
  postActiveGeneration : Nat
  preRouterGeneration : Nat
  postRouterGeneration : Nat
  preReadyGenerationCount : Nat
  postReadyGenerationCount : Nat
  preLiveGenerationCount : Nat
  postLiveGenerationCount : Nat
  preInFlightCount : Nat
  postInFlightCount : Nat
  trackedRequestId : RequestId
  trackedSessionId : SessionId
  trackedRequestGeneration : Generation
  trackedRequestSession : SessionId
  trackedRequestBehavior : BehaviorId
  trackedSessionBehavior : BehaviorId
  deriving Repr

structure SessionRecoveryCase where
  name : String
  action : String
  legal : Bool
  preLatestState : String
  postLatestState : String
  preLatestAdmission : String
  postLatestAdmission : String
  preFailedAdmission : String
  postFailedAdmission : String
  postNewAdmission : String
  failedId : RequestId
  newId : RequestId
  preLatestId : RequestId
  postLatestId : RequestId
  preSessionId : SessionId
  postSessionId : SessionId
  preBehaviorId : BehaviorId
  postBehaviorId : BehaviorId
  preRequestCount : Nat
  postRequestCount : Nat
  preRetryCount : Nat
  postRetryCount : Nat
  maxRetries : Nat
  preDeadlineExceeded : Bool
  postDeadlineExceeded : Bool
  preFailedIsLatest : Bool
  postFailedIsLatest : Bool
  postNewIsLatest : Bool
  preNewRequestExists : Bool
  oldRequestRetained : Bool
  newRequestInserted : Bool
  originPreserved : Bool
  backendPreserved : Bool
  deriving Repr

structure InferenceSlotAccountingCase where
  name : String
  property : String
  backendId : String
  preState : String
  postState : String
  contribution : Nat
  expectedContribution : Nat
  preContribution : Nat
  postContribution : Nat
  releasedSlot : Bool
  permitDropTerminalization : Bool
  rowStates : List String
  rowBackendIds : List String
  reconstructedRunningCount : Nat
  maxConcurrent : Nat
  boundedByMaxConcurrent : Bool
  deriving Repr

structure FleetSlotAccountingCase where
  name : String
  property : String
  backendId : String
  requestState : String
  admissionState : String
  contribution : Nat
  expectedContribution : Nat
  activeCount : Nat
  schedulerRunning : Nat
  slotCount : Nat
  maxConcurrent : Nat
  boundedByMaxConcurrent : Bool
  aggregateReconstructedNotPersisted : Bool
  deriving Repr

def boolString (value : Bool) : String :=
  if value then "true" else "false"

def runtimeResolvedA : ResolvedSnapshot :=
  { defaultBehavior := 10, runnable := {10}, unavailable := ∅ }

def runtimeResolvedB : ResolvedSnapshot :=
  { defaultBehavior := 20, runnable := {20}, unavailable := {10} }

def runtimeBoot : RuntimeState :=
  RuntimeState.bootState runtimeResolvedA

def runtimeApplyingChanged : RuntimeState :=
  { runtimeBoot with phase := .applying, pendingResolved := some runtimeResolvedB }

def runtimePublishedBeforeRouter : RuntimeState :=
  { runtimeBoot with
    lastResolved := runtimeResolvedB
  , active := runtimeResolvedB.activate 2
  , routerObservedGeneration := 1
  , readyGenerations := {1, 2}
  , liveGenerations := {1, 2}
  }

def runtimeRouterObserved : RuntimeState :=
  { runtimePublishedBeforeRouter with routerObservedGeneration := 2 }

def runtimeWithInFlight : RuntimeState :=
  { runtimeRouterObserved with
    inFlight := {500}
  , requestGeneration := Function.update runtimeRouterObserved.requestGeneration 500 2
  , requestSession := Function.update runtimeRouterObserved.requestSession 500 100
  , requestBehavior := Function.update runtimeRouterObserved.requestBehavior 500 20
  , sessionBehavior := Function.update runtimeRouterObserved.sessionBehavior 100 (some 20)
  }

def runtimeCaseFromStep
    (name actionName : String)
    (pre : RuntimeState)
    (action : RuntimeState.Action)
    (trackedRequestId : RequestId := 0)
    (trackedSessionId : SessionId := 0) : RuntimeReconcileCase :=
  match RuntimeState.step? pre action with
  | some post =>
      { name := name
      , action := actionName
      , legal := true
      , prePhase := pre.phase.toDefraDB
      , postPhase := post.phase.toDefraDB
      , preActiveGeneration := pre.active.generation
      , postActiveGeneration := post.active.generation
      , preRouterGeneration := pre.routerObservedGeneration
      , postRouterGeneration := post.routerObservedGeneration
      , preReadyGenerationCount := pre.readyGenerations.card
      , postReadyGenerationCount := post.readyGenerations.card
      , preLiveGenerationCount := pre.liveGenerations.card
      , postLiveGenerationCount := post.liveGenerations.card
      , preInFlightCount := pre.inFlight.card
      , postInFlightCount := post.inFlight.card
      , trackedRequestId := trackedRequestId
      , trackedSessionId := trackedSessionId
      , trackedRequestGeneration := post.requestGeneration trackedRequestId
      , trackedRequestSession := post.requestSession trackedRequestId
      , trackedRequestBehavior := post.requestBehavior trackedRequestId
      , trackedSessionBehavior :=
          match post.sessionBehavior trackedSessionId with
          | some behaviorId => behaviorId
          | none => 0
      }
  | none =>
      { name := name
      , action := actionName
      , legal := false
      , prePhase := pre.phase.toDefraDB
      , postPhase := ""
      , preActiveGeneration := pre.active.generation
      , postActiveGeneration := 0
      , preRouterGeneration := pre.routerObservedGeneration
      , postRouterGeneration := 0
      , preReadyGenerationCount := pre.readyGenerations.card
      , postReadyGenerationCount := 0
      , preLiveGenerationCount := pre.liveGenerations.card
      , postLiveGenerationCount := 0
      , preInFlightCount := pre.inFlight.card
      , postInFlightCount := 0
      , trackedRequestId := trackedRequestId
      , trackedSessionId := trackedSessionId
      , trackedRequestGeneration := 0
      , trackedRequestSession := 0
      , trackedRequestBehavior := 0
      , trackedSessionBehavior := 0
      }

def runtimeReconcileCases : List RuntimeReconcileCase :=
  [ runtimeCaseFromStep
      "publish_changed_snapshot"
      "publish"
      runtimeApplyingChanged
      (.publish runtimeResolvedB)
  , runtimeCaseFromStep
      "router_observe_published_generation"
      "routerObserve"
      runtimePublishedBeforeRouter
      .routerObserve
  , runtimeCaseFromStep
      "accept_request_after_router_observe"
      "acceptRequest"
      runtimeRouterObserved
      (.acceptRequest 100 500)
      500
      100
  , runtimeCaseFromStep
      "finish_request_releases_generation"
      "finishRequest"
      runtimeWithInFlight
      (.finishRequest 500)
      500
      100
  , runtimeCaseFromStep
      "retire_unobserved_generation"
      "retireGeneration"
      runtimeRouterObserved
      (.retireGeneration 1)
  , runtimeCaseFromStep
      "apply_failed_clears_pending"
      "applyFailed"
      runtimeApplyingChanged
      .applyFailed
  ]

def contractBackend : BackendId :=
  { val := "contract-backend" }

def otherBackend : BackendId :=
  { val := "other-backend" }

def admissionName : AdmissionState → String
  | .released => "released"
  | .waiting => "waiting"
  | .acquired => "acquired"
  | .executing => "executing"

def slotCall
    (callId : Nat)
    (backend : BackendId)
    (state : InferenceCallState) : InferenceCall :=
  { callId := callId
  , requestId := callId
  , backend := backend
  , state := state
  }

def inferenceRowsForBound : Nat → InferenceCall
  | 1 => slotCall 1 contractBackend .running
  | 2 => slotCall 2 contractBackend .queued
  | 3 => slotCall 3 contractBackend .completed
  | 4 => slotCall 4 otherBackend .running
  | n => slotCall n otherBackend .failed

def inferenceBoundedCallIds : Finset Nat :=
  {1, 2, 3, 4}

def inferenceBoundedRunningCount : Nat :=
  InferenceCall.reconstructedSlotCount
    inferenceBoundedCallIds
    inferenceRowsForBound
    contractBackend

def inferenceStateContributionCase
    (name : String)
    (state : InferenceCallState)
    (expected : Nat) : InferenceSlotAccountingCase :=
  let call := slotCall 1 contractBackend state
  let contribution := call.slotContribution contractBackend
  { name := name
  , property := "state_contribution"
  , backendId := contractBackend.val
  , preState := state.toDefraDB
  , postState := state.toDefraDB
  , contribution := contribution
  , expectedContribution := expected
  , preContribution := contribution
  , postContribution := contribution
  , releasedSlot := false
  , permitDropTerminalization := false
  , rowStates := [state.toDefraDB]
  , rowBackendIds := [contractBackend.val]
  , reconstructedRunningCount := contribution
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (contribution ≤ 1)
  }

def inferenceReleaseCase
    (name : String)
    (terminal : InferenceCallState) : InferenceSlotAccountingCase :=
  let pre := slotCall 1 contractBackend .running
  let post := slotCall 1 contractBackend terminal
  let preContribution := pre.slotContribution contractBackend
  let postContribution := post.slotContribution contractBackend
  { name := name
  , property := "terminal_release"
  , backendId := contractBackend.val
  , preState := InferenceCallState.running.toDefraDB
  , postState := terminal.toDefraDB
  , contribution := postContribution
  , expectedContribution := 0
  , preContribution := preContribution
  , postContribution := postContribution
  , releasedSlot := decide (preContribution = 1 ∧ postContribution = 0)
  , permitDropTerminalization := false
  , rowStates := [terminal.toDefraDB]
  , rowBackendIds := [contractBackend.val]
  , reconstructedRunningCount := postContribution
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (postContribution ≤ 1)
  }

def inferencePermitDropCase
    (name : String)
    (terminal : InferenceCallState) : InferenceSlotAccountingCase :=
  let pre := slotCall 1 contractBackend .running
  let post := slotCall 1 contractBackend terminal
  let preContribution := pre.slotContribution contractBackend
  let postContribution := post.slotContribution contractBackend
  { name := name
  , property := "permit_drop_terminalization"
  , backendId := contractBackend.val
  , preState := InferenceCallState.running.toDefraDB
  , postState := terminal.toDefraDB
  , contribution := postContribution
  , expectedContribution := 0
  , preContribution := preContribution
  , postContribution := postContribution
  , releasedSlot := decide (preContribution = 1 ∧ postContribution = 0)
  , permitDropTerminalization := true
  , rowStates := [terminal.toDefraDB]
  , rowBackendIds := [contractBackend.val]
  , reconstructedRunningCount := postContribution
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (postContribution ≤ 1)
  }

def inferenceBoundedCase : InferenceSlotAccountingCase :=
  { name := "reconstructed_running_count_bounded_by_max_concurrent"
  , property := "reconstructed_running_bound"
  , backendId := contractBackend.val
  , preState := ""
  , postState := ""
  , contribution := inferenceBoundedRunningCount
  , expectedContribution := 1
  , preContribution := 0
  , postContribution := 0
  , releasedSlot := false
  , permitDropTerminalization := false
  , rowStates :=
      [ InferenceCallState.running.toDefraDB
      , InferenceCallState.queued.toDefraDB
      , InferenceCallState.completed.toDefraDB
      , InferenceCallState.running.toDefraDB
      ]
  , rowBackendIds :=
      [ contractBackend.val
      , contractBackend.val
      , contractBackend.val
      , otherBackend.val
      ]
  , reconstructedRunningCount := inferenceBoundedRunningCount
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (inferenceBoundedRunningCount ≤ 1)
  }

def inferenceSlotAccountingCases : List InferenceSlotAccountingCase :=
  [ inferenceStateContributionCase "queued_contributes_zero" .queued 0
  , inferenceStateContributionCase "running_contributes_one" .running 1
  , inferenceStateContributionCase "cancelled_terminal_contributes_zero" .cancelled 0
  , inferenceStateContributionCase "completed_terminal_contributes_zero" .completed 0
  , inferenceStateContributionCase "failed_terminal_contributes_zero" .failed 0
  , inferenceReleaseCase "cancelled_releases_slot" .cancelled
  , inferenceReleaseCase "completed_releases_slot" .completed
  , inferenceReleaseCase "failed_releases_slot" .failed
  , inferencePermitDropCase "permit_drop_failed_terminalization_not_counted" .failed
  , inferencePermitDropCase "permit_drop_cancelled_terminalization_not_counted" .cancelled
  , inferenceBoundedCase
  ]

def slotContext
    (state : RequestState)
    (admission : AdmissionState)
    (backend : BackendId := contractBackend) : RequestContext :=
  { state := state
  , origin := .interactive
  , backend := backend
  , admission := admission
  , deadline := 10
  , claimTime := 0
  , currentTime := 0
  , retryCount := 0
  , maxRetries := 3
  , progressSeq := 0
  , messageSeq := 0
  , isLatest := true
  , persistence := .uncommitted
  }

def fleetRowsForBound : Nat → RequestContext
  | 1 => slotContext .claimed .acquired contractBackend
  | 2 => slotContext .processing .executing contractBackend
  | 3 => slotContext .claimed .waiting contractBackend
  | 4 => slotContext .completed .released contractBackend
  | _ => slotContext .processing .executing otherBackend

def fleetBoundedState : FleetState :=
  { activeIds := {1, 2, 3, 4}
  , ctx := fleetRowsForBound
  , scheduler :=
      { running := fun bid => if bid = contractBackend then 2 else 0
      , backends := fun bid =>
          if bid = contractBackend then
            { max_concurrent := 2, available := true }
          else
            { max_concurrent := 1, available := true }
      }
  }

def fleetSlotContributionCase
    (name : String)
    (state : RequestState)
    (admission : AdmissionState)
    (expected : Nat) : FleetSlotAccountingCase :=
  let ctx := slotContext state admission contractBackend
  let contribution := FleetState.slotContribution ctx contractBackend
  { name := name
  , property := "admission_contribution"
  , backendId := contractBackend.val
  , requestState := state.toDefraDB
  , admissionState := admissionName admission
  , contribution := contribution
  , expectedContribution := expected
  , activeCount := 1
  , schedulerRunning := contribution
  , slotCount := contribution
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (contribution ≤ 1)
  , aggregateReconstructedNotPersisted := true
  }

def fleetBoundedCase : FleetSlotAccountingCase :=
  let slotCount := fleetBoundedState.slotCountFor contractBackend
  let schedulerRunning := fleetBoundedState.scheduler.running contractBackend
  let maxConcurrent := (fleetBoundedState.scheduler.backends contractBackend).max_concurrent
  { name := "fleet_reconstructed_running_count_bounded_by_max_concurrent"
  , property := "fleet_reconstructed_running_bound"
  , backendId := contractBackend.val
  , requestState := ""
  , admissionState := ""
  , contribution := slotCount
  , expectedContribution := 2
  , activeCount := fleetBoundedState.activeIds.card
  , schedulerRunning := schedulerRunning
  , slotCount := slotCount
  , maxConcurrent := maxConcurrent
  , boundedByMaxConcurrent := decide (slotCount ≤ maxConcurrent)
  , aggregateReconstructedNotPersisted := true
  }

def fleetSlotAccountingCases : List FleetSlotAccountingCase :=
  [ fleetSlotContributionCase "fleet_waiting_contributes_zero" .claimed .waiting 0
  , fleetSlotContributionCase "fleet_acquired_contributes_one" .claimed .acquired 1
  , fleetSlotContributionCase "fleet_executing_contributes_one" .processing .executing 1
  , fleetSlotContributionCase "fleet_released_terminal_contributes_zero" .completed .released 0
  , fleetBoundedCase
  ]

def recoveryContext
    (state : RequestState)
    (admission : AdmissionState)
    (retryCount maxRetries deadline currentTime : Nat)
    (isLatest : Bool) : RequestContext :=
  { state := state
  , origin := .interactive
  , backend := contractBackend
  , admission := admission
  , deadline := deadline
  , claimTime := currentTime
  , currentTime := currentTime
  , retryCount := retryCount
  , maxRetries := maxRetries
  , progressSeq := 3
  , messageSeq := 7
  , isLatest := isLatest
  , persistence := .committed
  }

def recoveryPre
    (failedCtx latestCtx : RequestContext)
    (latestId : RequestId := 1) : SessionState :=
  { sessionId := 10
  , behaviorId := 20
  , requestIds := {1, 3}
  , ctx := fun rid =>
      if rid = 1 then failedCtx
      else if rid = 3 then latestCtx
      else recoveryContext .pending .released 0 3 10 0 false
  , latest := latestId
  }

def recoveryAdmissionName : AdmissionState → String
  | .released => "released"
  | .waiting => "waiting"
  | .acquired => "acquired"
  | .executing => "executing"

def recoveryCaseFromStep
    (name : String)
    (pre : SessionState)
    (failedId newId : RequestId) : SessionRecoveryCase :=
  let failedPre := pre.ctx failedId
  let latestPre := pre.ctx pre.latest
  match SessionState.step? pre (.reissueFailed failedId newId) with
  | some post =>
      let failedPost := post.ctx failedId
      let newPost := post.ctx newId
      let latestPost := post.ctx post.latest
      { name := name
      , action := "reissueFailed"
      , legal := true
      , preLatestState := latestPre.state.toDefraDB
      , postLatestState := latestPost.state.toDefraDB
      , preLatestAdmission := recoveryAdmissionName latestPre.admission
      , postLatestAdmission := recoveryAdmissionName latestPost.admission
      , preFailedAdmission := recoveryAdmissionName failedPre.admission
      , postFailedAdmission := recoveryAdmissionName failedPost.admission
      , postNewAdmission := recoveryAdmissionName newPost.admission
      , failedId := failedId
      , newId := newId
      , preLatestId := pre.latest
      , postLatestId := post.latest
      , preSessionId := pre.sessionId
      , postSessionId := post.sessionId
      , preBehaviorId := pre.behaviorId
      , postBehaviorId := post.behaviorId
      , preRequestCount := pre.requestIds.card
      , postRequestCount := post.requestIds.card
      , preRetryCount := failedPre.retryCount
      , postRetryCount := newPost.retryCount
      , maxRetries := failedPre.maxRetries
      , preDeadlineExceeded := decide failedPre.deadlineExceeded
      , postDeadlineExceeded := decide newPost.deadlineExceeded
      , preFailedIsLatest := failedPre.isLatest
      , postFailedIsLatest := failedPost.isLatest
      , postNewIsLatest := newPost.isLatest
      , preNewRequestExists := decide (newId ∈ pre.requestIds)
      , oldRequestRetained := decide (failedId ∈ post.requestIds)
      , newRequestInserted := decide (newId ∈ post.requestIds)
      , originPreserved := decide (newPost.origin = failedPre.origin)
      , backendPreserved := decide (newPost.backend = failedPre.backend)
      }
  | none =>
      { name := name
      , action := "reissueFailed"
      , legal := false
      , preLatestState := latestPre.state.toDefraDB
      , postLatestState := ""
      , preLatestAdmission := recoveryAdmissionName latestPre.admission
      , postLatestAdmission := ""
      , preFailedAdmission := recoveryAdmissionName failedPre.admission
      , postFailedAdmission := ""
      , postNewAdmission := ""
      , failedId := failedId
      , newId := newId
      , preLatestId := pre.latest
      , postLatestId := 0
      , preSessionId := pre.sessionId
      , postSessionId := 0
      , preBehaviorId := pre.behaviorId
      , postBehaviorId := 0
      , preRequestCount := pre.requestIds.card
      , postRequestCount := 0
      , preRetryCount := failedPre.retryCount
      , postRetryCount := 0
      , maxRetries := failedPre.maxRetries
      , preDeadlineExceeded := decide failedPre.deadlineExceeded
      , postDeadlineExceeded := false
      , preFailedIsLatest := failedPre.isLatest
      , postFailedIsLatest := false
      , postNewIsLatest := false
      , preNewRequestExists := decide (newId ∈ pre.requestIds)
      , oldRequestRetained := false
      , newRequestInserted := false
      , originPreserved := false
      , backendPreserved := false
      }

def sessionRecoveryCases : List SessionRecoveryCase :=
  let initialFailed := recoveryContext .failed .released 0 3 10 5 true
  let openFailed := recoveryContext .failed .released 1 3 10 5 true
  let lastBudgetFailed := recoveryContext .failed .released 2 3 10 5 true
  let exhaustedFailed := recoveryContext .failed .released 3 3 10 5 true
  let deadlineClosedFailed := recoveryContext .failed .released 1 3 10 11 true
  let nonLatestFailed := recoveryContext .failed .released 1 3 10 5 false
  let latestFailed := recoveryContext .failed .released 0 3 10 5 true
  let pendingLatest := recoveryContext .pending .released 1 3 10 5 true
  let claimedLatest := recoveryContext .failed .waiting 1 3 10 5 true
  [ recoveryCaseFromStep
      "legal_initial_retry_slot"
      (recoveryPre initialFailed initialFailed 1)
      1
      2
  , recoveryCaseFromStep
      "legal_open_budget_latest"
      (recoveryPre openFailed openFailed 1)
      1
      2
  , recoveryCaseFromStep
      "legal_last_retry_slot"
      (recoveryPre lastBudgetFailed lastBudgetFailed 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_retry_budget_exhausted"
      (recoveryPre exhaustedFailed exhaustedFailed 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_deadline_closed"
      (recoveryPre deadlineClosedFailed deadlineClosedFailed 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_non_latest_failed_request"
      (recoveryPre nonLatestFailed latestFailed 3)
      1
      2
  , recoveryCaseFromStep
      "illegal_new_request_id_already_exists"
      (recoveryPre openFailed openFailed 1)
      1
      3
  , recoveryCaseFromStep
      "illegal_new_request_id_matches_failed_id"
      (recoveryPre openFailed openFailed 1)
      1
      1
  , recoveryCaseFromStep
      "illegal_source_not_failed"
      (recoveryPre pendingLatest pendingLatest 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_source_not_released"
      (recoveryPre claimedLatest claimedLatest 1)
      1
      2
  ]

end Conformance.ContractCases
