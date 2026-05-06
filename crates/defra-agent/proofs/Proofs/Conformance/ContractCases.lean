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
  oldRequestRetained : Bool
  newRequestInserted : Bool
  originPreserved : Bool
  backendPreserved : Bool
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

def recoveryCaseFromStep
    (name : String)
    (pre : SessionState)
    (failedId newId : RequestId) : SessionRecoveryCase :=
  let failedPre := pre.ctx failedId
  match SessionState.step? pre (.reissueFailed failedId newId) with
  | some post =>
      let failedPost := post.ctx failedId
      let newPost := post.ctx newId
      { name := name
      , action := "reissueFailed"
      , legal := true
      , preLatestState := (pre.ctx pre.latest).state.toDefraDB
      , postLatestState := (post.ctx post.latest).state.toDefraDB
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
      , oldRequestRetained := decide (failedId ∈ post.requestIds)
      , newRequestInserted := decide (newId ∈ post.requestIds)
      , originPreserved := decide (newPost.origin = failedPre.origin)
      , backendPreserved := decide (newPost.backend = failedPre.backend)
      }
  | none =>
      { name := name
      , action := "reissueFailed"
      , legal := false
      , preLatestState := (pre.ctx pre.latest).state.toDefraDB
      , postLatestState := ""
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
      , oldRequestRetained := false
      , newRequestInserted := false
      , originPreserved := false
      , backendPreserved := false
      }

def sessionRecoveryCases : List SessionRecoveryCase :=
  let openFailed := recoveryContext .failed .released 1 3 10 5 true
  let lastBudgetFailed := recoveryContext .failed .released 2 3 10 5 true
  let exhaustedFailed := recoveryContext .failed .released 3 3 10 5 true
  let deadlineClosedFailed := recoveryContext .failed .released 1 3 10 11 true
  let nonLatestFailed := recoveryContext .failed .released 1 3 10 5 false
  let latestFailed := recoveryContext .failed .released 0 3 10 5 true
  let pendingLatest := recoveryContext .pending .released 1 3 10 5 true
  let claimedLatest := recoveryContext .failed .waiting 1 3 10 5 true
  [ recoveryCaseFromStep
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
