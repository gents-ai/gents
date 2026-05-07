import Proofs.Conformance.ContractCases.Types

/-!
# Session Recovery Witness Cases
-/

namespace Conformance.ContractCases

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
