import Proofs.Request
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.ContractCases.Types

namespace Conformance.Contracts

open Conformance.ContractCases

def requestStates : List RequestState :=
  [ .workspaceBindingPending, .pending, .claimed, .processing, .waitingForBackend, .resumingBackend, .completed
  , .failed, .superseded, .dead, .interrupted ]

def requestStateNames : List String :=
  requestStates.map RequestState.toDefraDB

def requestActions : List (String × RequestContext.Action) :=
  [ ("bindWorkspace", .bindWorkspace)
  , ("claim", .claim)
  , ("dedupLose", .dedupLose)
  , ("admissionReject", .admissionReject)
  , ("beginInference", .beginInference)
  , ("continueProcessing", .continueProcessing)
  , ("parkBackendClosed", .parkBackend true)
  , ("parkBackendUnclosed", .parkBackend false)
  , ("resumeBackendReady", .resumeBackend true)
  , ("resumeBackendNotReady", .resumeBackend false)
  , ("beginResumed", .beginResumed)
  , ("interruptResuming", .interruptResuming)
  , ("expireResuming", .expireResuming)
  , ("interruptBackendWait", .interruptBackendWait)
  , ("expireBackendWait", .expireBackendWait)
  , ("finish", .finish)
  , ("fail", .fail)
  , ("failBeforeStream", .failBeforeStream)
  , ("expire", .expire)
  , ("interruptBeforeClaim", .interruptBeforeClaim)
  , ("interruptClaimed", .interruptClaimed)
  , ("interruptProcessing", .interruptProcessing)
  ]

def requestContext
    (state : RequestState)
    (admission : AdmissionState)
    (hasInterrupt : Bool := false)
    (validUntil : Option Time := none)
    (currentTime : Time := 0) : RequestContext :=
  { state := state
  , origin := .interactive
  , backend := contractBackend
  , admission := admission
  , deadline := 10
  , claimTime := 0
  , currentTime := currentTime
  , retryCount := 0
  , maxRetries := 3
  , messageSeq := 0
  , persistence := .uncommitted
  , interruptRequestedAt := if hasInterrupt then some currentTime else none
  , validUntil := validUntil
  }

def requestSamples : List RequestContext :=
  [ requestContext .workspaceBindingPending .released
  , requestContext .pending .released
  , requestContext .pending .released true
  , requestContext .pending .released false (some 0) 1
  , requestContext .claimed .waiting
  , requestContext .claimed .acquired
  , requestContext .claimed .waiting true
  , requestContext .claimed .acquired true
  , requestContext .processing .executing
  , requestContext .processing .executing true
  , requestContext .resumingBackend .waiting
  , requestContext .resumingBackend .acquired
  , requestContext .resumingBackend .waiting true
  , requestContext .resumingBackend .acquired false none 10
  , requestContext .waitingForBackend .released
  , requestContext .waitingForBackend .released true
  , { (requestContext .waitingForBackend .released false none 2) with backendRetryAt := some 1 }
  , requestContext .waitingForBackend .released false none 10
  , requestContext .completed .released
  , requestContext .failed .released
  , requestContext .superseded .released
  , requestContext .dead .released
  , requestContext .interrupted .released
  ]

def requestMachine : StateMachineContract :=
  machineContract
    "Request"
    requestStateNames
    (terminalNames requestStates RequestState.toDefraDB)
    (actionNames requestActions)
    (transitionPairsFromSamples
      requestSamples
      requestActions
      RequestContext.step?
      (fun ctx => ctx.state.toDefraDB))

/-- A parked attempt releases its slot only after its output is durably closed. -/
theorem backend_wait_requires_closed_attempt :
    (RequestContext.step? (requestContext .processing .executing) (.parkBackend false)).isNone = true := by
  native_decide

theorem backend_wait_preserves_deadline_and_retry_budget :
    (RequestContext.step? (requestContext .processing .executing) (.parkBackend true)).map
      (fun r => (r.state, r.admission, r.deadline, r.retryCount, r.backendAttempts)) =
      some (.waitingForBackend, .released, 10, 0, 1) := by
  native_decide

theorem backend_wait_records_fallback_in_the_request :
    (RequestContext.step?
      { (requestContext .processing .executing) with deadline := 10000 }
      (.parkBackend true)).bind (·.backendRetryAt) = some 1000 := by
  native_decide

theorem backend_resume_preserves_deadline_after_initial_ttl :
    (RequestContext.step? (requestContext .waitingForBackend .released false (some 0) 1)
      (.resumeBackend true)).map (fun r => (r.state, r.deadline, r.retryCount)) =
      some (.resumingBackend, 10, 0) := by
  native_decide

theorem backend_wait_does_not_resume_without_wake :
    (RequestContext.step? (requestContext .waitingForBackend .released) (.resumeBackend false)).isNone = true := by
  native_decide

theorem backend_wait_resumes_on_due_fallback :
    (RequestContext.step?
      { (requestContext .waitingForBackend .released false none 2) with
        backendRetryAt := some 1 } (.resumeBackend false)).map (·.state) =
      some .resumingBackend := by
  native_decide

theorem backend_wait_rejects_resume_on_cancel_or_deadline :
    (RequestContext.step? (requestContext .waitingForBackend .released true) (.resumeBackend true)).isNone = true ∧
    (RequestContext.step? (requestContext .waitingForBackend .released false none 10)
      (.resumeBackend true)).isNone = true := by
  native_decide

end Conformance.Contracts
