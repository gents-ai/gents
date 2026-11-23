import Proofs.Session.Request
import Proofs.Session.SessionRecovery
import Proofs.Conformance.ContractCases.Types
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.ContractCases

def recoveryContextWith
    (origin : ExecutionOrigin)
    (state : RequestState)
    (admission : AdmissionState)
    (retryCount maxRetries deadline currentTime : Nat) : SessionRetry.Request :=
  { state := state
  , origin := origin
  , admission := admission
  , deadline := some deadline
  , currentTime := currentTime
  , retryCount := retryCount
  , maxRetries := maxRetries
  }

def recoveryContext
    (state : RequestState)
    (admission : AdmissionState)
    (retryCount maxRetries deadline currentTime : Nat) : SessionRetry.Request :=
  recoveryContextWith .interactive
    state admission retryCount maxRetries deadline currentTime

def recoveryPre
    (failedCtx latestCtx : SessionRetry.Request)
    (latestId : RequestId := 1)
    (requestIds : Finset RequestId := {1, 3}) : SessionState :=
  { sessionId := 10
  , agentDid := 20
  , requestIds := requestIds
  , ctx := fun rid =>
      if rid = 1 then failedCtx
      else if rid = 3 then latestCtx
      else recoveryContext .pending .released 0 3 10 0
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
      , failedId := failedId
      , newId := newId
      , preLatestId := pre.latest
      , postLatestId := post.latest
      , preSessionId := pre.sessionId
      , postSessionId := post.sessionId
      , preAgentDid := pre.agentDid
      , postAgentDid := post.agentDid
      , preRequestCount := pre.requestIds.card
      , postRequestCount := post.requestIds.card
      , preRetryCount := failedPre.retryCount
      , postRetryCount := newPost.retryCount
      , maxRetries := failedPre.maxRetries
      , preDeadlineExceeded := decide failedPre.deadlineExceeded
      , postDeadlineExceeded := decide newPost.deadlineExceeded
      , preFailedIsLatest := (failedId == pre.latest)
      , postFailedIsLatest := (failedId == post.latest)
      , postNewIsLatest := (newId == post.latest)
      , preRequestIds := requestIdList pre.requestIds
      , preFailedExists := decide (failedId ∈ pre.requestIds)
      , preLatestExists := decide (pre.latest ∈ pre.requestIds)
      , preNewRequestExists := decide (newId ∈ pre.requestIds)
      , oldRequestRetained := decide (failedId ∈ post.requestIds)
      , newRequestInserted := decide (newId ∈ post.requestIds)
      , originPreserved := decide (newPost.origin = failedPre.origin)
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
      , failedId := failedId
      , newId := newId
      , preLatestId := pre.latest
      , postLatestId := 0
      , preSessionId := pre.sessionId
      , postSessionId := 0
      , preAgentDid := pre.agentDid
      , postAgentDid := 0
      , preRequestCount := pre.requestIds.card
      , postRequestCount := 0
      , preRetryCount := failedPre.retryCount
      , postRetryCount := 0
      , maxRetries := failedPre.maxRetries
      , preDeadlineExceeded := decide failedPre.deadlineExceeded
      , postDeadlineExceeded := false
      , preFailedIsLatest := (failedId == pre.latest)
      , postFailedIsLatest := false
      , postNewIsLatest := false
      , preRequestIds := requestIdList pre.requestIds
      , preFailedExists := decide (failedId ∈ pre.requestIds)
      , preLatestExists := decide (pre.latest ∈ pre.requestIds)
      , preNewRequestExists := decide (newId ∈ pre.requestIds)
      , oldRequestRetained := false
      , newRequestInserted := false
      , originPreserved := false
      }

deriving instance Lean.ToJson for ExecutionOrigin, AdmissionState

structure RecoveryRequest where
  state : RequestState
  origin : ExecutionOrigin
  admission : AdmissionState
  retryCount : Nat
  maxRetries : Nat
  deadline : Nat
  currentTime : Nat
  deriving Lean.ToJson

def RecoveryRequest.toModel (r : RecoveryRequest) : SessionRetry.Request :=
  recoveryContextWith r.origin r.state r.admission r.retryCount r.maxRetries r.deadline
    r.currentTime

def recoveryRequest (state : RequestState) (admission : AdmissionState)
    (retryCount currentTime : Nat) (origin : ExecutionOrigin := .interactive) : RecoveryRequest :=
  { state, origin, admission, retryCount, maxRetries := 3, deadline := 10, currentTime }

structure RecoveryInput where
  name : String
  failed : RecoveryRequest
  latest : RecoveryRequest
  latestId : Nat
  requestIds : List Nat
  failedId : Nat
  newId : Nat
  deriving Lean.ToJson

def RecoveryInput.pre (i : RecoveryInput) : SessionState :=
  recoveryPre i.failed.toModel i.latest.toModel i.latestId i.requestIds.toFinset

def recoveryInputs : List RecoveryInput :=
  let initialFailed := recoveryRequest .failed .released 0 5
  let openFailed := recoveryRequest .failed .released 1 5
  let pendingLatest := recoveryRequest .pending .released 1 5
  let recoveryCase (name : String) (failed latest : RecoveryRequest) (latestId : Nat := 1)
      (requestIds : List Nat := [1, 3]) (newId : Nat := 2) : RecoveryInput :=
    { name, failed, latest, latestId, requestIds, failedId := 1, newId }
  [ recoveryCase "legal_initial_retry_slot" initialFailed initialFailed
  , let f := recoveryRequest .failed .released 1 5 .scheduled
    recoveryCase "illegal_automated_origin" f f
  , let f := recoveryRequest .failed .released 2 5
    recoveryCase "legal_last_retry_slot" f f
  , let f := recoveryRequest .failed .released 3 5
    recoveryCase "illegal_retry_budget_exhausted" f f
  , let f := recoveryRequest .failed .released 1 11
    recoveryCase "illegal_deadline_closed" f f
  , recoveryCase "illegal_non_latest_failed_request" openFailed initialFailed (latestId := 3)
  , recoveryCase "illegal_non_latest_failed_with_pending_latest" openFailed pendingLatest (latestId := 3)
  , recoveryCase "illegal_new_request_id_already_exists" openFailed openFailed (newId := 3)
  , recoveryCase "illegal_new_request_id_matches_failed_id" openFailed openFailed (newId := 1)
  , recoveryCase "illegal_source_not_failed" pendingLatest pendingLatest
  , let f := recoveryRequest .completed .released 1 5
    recoveryCase "illegal_source_completed_terminal" f f
  , let f := recoveryRequest .dead .released 1 5
    recoveryCase "illegal_source_dead_stale_terminal" f f
  , let f := recoveryRequest .superseded .released 1 5
    recoveryCase "illegal_source_superseded_terminal" f f
  , let f := recoveryRequest .interrupted .released 1 5
    recoveryCase "illegal_source_interrupted_terminal" f f
  , let f := recoveryRequest .processing .executing 1 5
    recoveryCase "illegal_source_processing_active_runtime" f f
  , recoveryCase "illegal_missing_failed_request" openFailed pendingLatest (requestIds := [3]) ]

def sessionRecoveryCases : List SessionRecoveryCase :=
  recoveryInputs.map fun i => recoveryCaseFromStep i.name i.pre i.failedId i.newId

end Conformance.ContractCases
