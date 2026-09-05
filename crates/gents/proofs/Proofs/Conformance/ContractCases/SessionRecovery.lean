import Proofs.Request
import Proofs.SessionRecovery
import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

def recoveryContextWith
    (origin : ExecutionOrigin)
    (backend : BackendId)
    (state : RequestState)
    (admission : AdmissionState)
    (retryCount maxRetries deadline currentTime : Nat)
    (isLatest : Bool) : RequestContext :=
  { state := state
  , origin := origin
  , backend := backend
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

def recoveryContext
    (state : RequestState)
    (admission : AdmissionState)
    (retryCount maxRetries deadline currentTime : Nat)
    (isLatest : Bool) : RequestContext :=
  recoveryContextWith .interactive contractBackend
    state admission retryCount maxRetries deadline currentTime isLatest

def recoveryBackendAlt : BackendId :=
  { val := "contract-backend-alt" }

def recoveryPre
    (failedCtx latestCtx : RequestContext)
    (latestId : RequestId := 1)
    (requestIds : Finset RequestId := {1, 3}) : SessionState :=
  { sessionId := 10
  , behaviorId := 20
  , requestIds := requestIds
  , ctx := fun rid =>
      if rid = 1 then failedCtx
      else if rid = 3 then latestCtx
      else recoveryContext .pending .released 0 3 10 0 false
  , latest := latestId
  }

def requestIdList (ids : Finset RequestId) : List RequestId :=
  [1, 2, 3].filter fun rid => decide (rid ∈ ids)

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
      , preFailedState := failedPre.state.toDefraDB
      , postLatestState := latestPost.state.toDefraDB
      , postFailedState := failedPost.state.toDefraDB
      , postNewState := newPost.state.toDefraDB
      , preLatestAdmission := admissionName latestPre.admission
      , postLatestAdmission := admissionName latestPost.admission
      , preFailedAdmission := admissionName failedPre.admission
      , postFailedAdmission := admissionName failedPost.admission
      , postNewAdmission := admissionName newPost.admission
      , preOrigin := failedPre.origin.toDefraDB
      , postNewOrigin := newPost.origin.toDefraDB
      , preBackend := failedPre.backend.val
      , postNewBackend := newPost.backend.val
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
      , preRequestIds := requestIdList pre.requestIds
      , preFailedExists := decide (failedId ∈ pre.requestIds)
      , preLatestExists := decide (pre.latest ∈ pre.requestIds)
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
      , preFailedState := failedPre.state.toDefraDB
      , postLatestState := ""
      , postFailedState := ""
      , postNewState := ""
      , preLatestAdmission := admissionName latestPre.admission
      , postLatestAdmission := ""
      , preFailedAdmission := admissionName failedPre.admission
      , postFailedAdmission := ""
      , postNewAdmission := ""
      , preOrigin := failedPre.origin.toDefraDB
      , postNewOrigin := ""
      , preBackend := failedPre.backend.val
      , postNewBackend := ""
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
      , preRequestIds := requestIdList pre.requestIds
      , preFailedExists := decide (failedId ∈ pre.requestIds)
      , preLatestExists := decide (pre.latest ∈ pre.requestIds)
      , preNewRequestExists := decide (newId ∈ pre.requestIds)
      , oldRequestRetained := false
      , newRequestInserted := false
      , originPreserved := false
      , backendPreserved := false
      }

def sessionRecoveryCases : List SessionRecoveryCase :=
  let initialFailed := recoveryContext .failed .released 0 3 10 5 true
  let openFailed := recoveryContext .failed .released 1 3 10 5 true
  let scheduledOpenFailed :=
    recoveryContextWith .scheduled recoveryBackendAlt .failed .released 1 3 10 5 true
  let lastBudgetFailed := recoveryContext .failed .released 2 3 10 5 true
  let exhaustedFailed := recoveryContext .failed .released 3 3 10 5 true
  let deadlineClosedFailed := recoveryContext .failed .released 1 3 10 11 true
  let nonLatestFailed := recoveryContext .failed .released 1 3 10 5 false
  let latestFailed := recoveryContext .failed .released 0 3 10 5 true
  let pendingLatest := recoveryContext .pending .released 1 3 10 5 true
  let completedLatest := recoveryContext .completed .released 1 3 10 5 true
  let deadLatest := recoveryContext .dead .released 1 3 10 5 true
  let supersededLatest := recoveryContext .superseded .released 1 3 10 5 true
  let interruptedLatest := recoveryContext .interrupted .released 1 3 10 5 true
  let inputRequiredLatest := recoveryContext .inputRequired .executing 1 3 10 5 true
  let processingLatest := recoveryContext .processing .executing 1 3 10 5 true
  [ recoveryCaseFromStep
      "legal_initial_retry_slot"
      (recoveryPre initialFailed initialFailed 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_automated_origin"
      (recoveryPre scheduledOpenFailed scheduledOpenFailed 1)
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
      "illegal_non_latest_failed_with_pending_latest"
      (recoveryPre nonLatestFailed pendingLatest 3)
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
      "illegal_source_completed_terminal"
      (recoveryPre completedLatest completedLatest 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_source_dead_stale_terminal"
      (recoveryPre deadLatest deadLatest 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_source_superseded_terminal"
      (recoveryPre supersededLatest supersededLatest 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_source_interrupted_terminal"
      (recoveryPre interruptedLatest interruptedLatest 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_source_input_required_reserved"
      (recoveryPre inputRequiredLatest inputRequiredLatest 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_source_processing_active_runtime"
      (recoveryPre processingLatest processingLatest 1)
      1
      2
  , recoveryCaseFromStep
      "illegal_missing_failed_request"
      (recoveryPre openFailed pendingLatest 1 {3})
      1
      2
  ]

end Conformance.ContractCases
