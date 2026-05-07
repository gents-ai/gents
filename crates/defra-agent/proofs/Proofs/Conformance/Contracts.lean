import Proofs.Request
import Proofs.Process
import Proofs.Persistence
import Proofs.StorageObservation
import Proofs.SessionRecovery
import Proofs.InferenceCall
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Triggers.Contracts
import Proofs.RuntimeReconcile
import Proofs.Conformance.ContractCases
import Proofs.ToolExecution
import Proofs.Conformance.CoverageLedger

/-!
# Rust Conformance Contracts

This module is the Lean-owned extraction surface for Rust conformance tests.
Rust runs this file with `lake env lean --run` and consumes the JSON emitted by
`main`. State vocabularies, transition tables, and finite witness rows are
evaluated from the Lean constructors, `toDefraDB` functions, and executable
`step?` functions below.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def requestStates : List RequestState :=
  [ .pending, .claimed, .processing, .inputRequired, .completed
  , .failed, .superseded, .dead, .interrupted ]

def requestStateNames : List String :=
  requestStates.map RequestState.toDefraDB

def requestActions : List (String × RequestContext.Action) :=
  [ ("claim", .claim)
  , ("dedupLose", .dedupLose)
  , ("beginInference", .beginInference)
  , ("advance", .advance)
  , ("finish", .finish)
  , ("fail", .fail)
  , ("failBeforeStream", .failBeforeStream)
  , ("expire", .expire)
  , ("interruptBeforeClaim", .interruptBeforeClaim)
  , ("interruptClaimed", .interruptClaimed)
  , ("interruptProcessing", .interruptProcessing)
  ]

def contractBackend : BackendId :=
  { val := "contract-backend" }

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
  , progressSeq := 0
  , messageSeq := 0
  , isLatest := true
  , persistence := .uncommitted
  , interruptRequestedAt := if hasInterrupt then some currentTime else none
  , validUntil := validUntil
  }

def requestSamples : List RequestContext :=
  [ requestContext .pending .released
  , requestContext .pending .released true
  , requestContext .pending .released false (some 0) 1
  , requestContext .claimed .waiting
  , requestContext .claimed .acquired
  , requestContext .claimed .waiting true
  , requestContext .claimed .acquired true
  , requestContext .processing .executing
  , requestContext .processing .executing true
  , requestContext .inputRequired .executing
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

def processStates : List ProcessState :=
  [ .uninitialized, .recovering, .ready, .shuttingDown, .shutdown ]

def processStateNames : List String :=
  processStates.map ProcessState.toDefraDB

def processActions : List (String × ProcessState.Action) :=
  [ ("startupRecover", .startupRecover { hasStuckRequests := true, activeRequestCount := 1 })
  , ("startupClean", .startupClean { hasStuckRequests := false, activeRequestCount := 0 })
  , ("recoveryComplete", .recoveryComplete)
  , ("beginShutdown", .beginShutdown)
  , ("finishShutdown", .finishShutdown 0)
  ]

def processMachine : StateMachineContract :=
  machineContract
    "Process"
    processStateNames
    (terminalNames processStates ProcessState.toDefraDB)
    (actionNames processActions)
    (transitionPairsFromSamples
      processStates
      processActions
      ProcessState.step?
      ProcessState.toDefraDB)

def persistenceStates : List PersistenceState :=
  [ .uncommitted, .committing, .committed, .lost ]

def persistenceStateNames : List String :=
  persistenceStates.map PersistenceState.toDefraDB

def persistenceActions : List (String × PersistenceState.Action) :=
  [ ("flush", .flush)
  , ("writeSuccess", .writeSuccess)
  , ("writeFail", .writeFail)
  , ("accumulate", .accumulate)
  ]

def persistenceMachine
    (domain : String)
    (policy : PersistenceState.FailurePolicy) : StateMachineContract :=
  machineContract
    domain
    persistenceStateNames
    (terminalNames persistenceStates PersistenceState.toDefraDB)
    (actionNames persistenceActions)
    (transitionPairsFromSamples
      persistenceStates
      persistenceActions
      (fun state action => PersistenceState.step? policy state action)
      PersistenceState.toDefraDB)

def storageObservationStates : List StorageObservation :=
  [ .noMutation
  , .inFlight
  , .successAcknowledged
  , .mutationFailed
  , .staleObserved
  , .readVisible
  , .lostAcknowledged
  ]

def storageObservationStateNames : List String :=
  storageObservationStates.map StorageObservation.toContract

def storageObservationActions : List (String × StorageObservation.Action) :=
  [ ("beginMutation", .beginMutation)
  , ("mutationSuccess", .mutationSuccess)
  , ("mutationFailure", .mutationFailure)
  , ("staleRead", .staleRead)
  , ("staleEvent", .staleEvent)
  , ("readYourWrites", .readYourWrites)
  , ("eventArrives", .eventArrives)
  , ("retryFailClosed", .retryFailClosed)
  , ("acknowledgeLost", .acknowledgeLost)
  ]

def storageObservationMachine
    (domain : String)
    (policy : PersistenceState.FailurePolicy) : StateMachineContract :=
  machineContract
    domain
    storageObservationStateNames
    (terminalNames storageObservationStates StorageObservation.toContract)
    (actionNames storageObservationActions)
    (transitionPairsFromSamples
      storageObservationStates
      storageObservationActions
      (fun state action => StorageObservation.step? policy state action)
      StorageObservation.toContract)

def runtimeReconcileStates : List ReconcilePhase :=
  [ .idle, .debouncing, .resolving, .diffing, .applying ]

def runtimeReconcileStateNames : List String :=
  runtimeReconcileStates.map ReconcilePhase.toDefraDB

def runtimeAckedChanged : RuntimeState :=
  { runtimeBoot with ackedResolved := some runtimeResolvedB }

def runtimeDebouncingChanged : RuntimeState :=
  { runtimeAckedChanged with
    phase := .debouncing
  , observedResolved := some runtimeResolvedB
  }

def runtimeResolvingChanged : RuntimeState :=
  { runtimeDebouncingChanged with phase := .resolving }

def runtimeDiffingNoop : RuntimeState :=
  { runtimeBoot with
    phase := .diffing
  , observedResolved := some runtimeResolvedA
  , pendingResolved := some runtimeResolvedA
  }

def runtimeDiffingChanged : RuntimeState :=
  { runtimeResolvingChanged with
    phase := .diffing
  , pendingResolved := some runtimeResolvedB
  }

def runtimeReconcileSamples : List RuntimeState :=
  [ runtimeBoot
  , runtimeAckedChanged
  , runtimeDebouncingChanged
  , runtimeResolvingChanged
  , runtimeDiffingNoop
  , runtimeDiffingChanged
  , runtimeApplyingChanged
  , runtimePublishedBeforeRouter
  , runtimeRouterObserved
  , runtimeWithInFlight
  ]

def runtimeReconcileActions : List (String × RuntimeState.Action) :=
  [ ("ackWrite", .ackWrite runtimeResolvedB)
  , ("observeDoc", .observeDoc runtimeResolvedB)
  , ("startResolve", .startResolve)
  , ("resolveVisible", .resolveVisible runtimeResolvedB)
  , ("diffNoop", .diffNoop runtimeResolvedA)
  , ("beginApply", .beginApply runtimeResolvedB)
  , ("publish", .publish runtimeResolvedB)
  , ("applyFailed", .applyFailed)
  , ("routerObserve", .routerObserve)
  , ("acceptRequest", .acceptRequest 100 500)
  , ("finishRequest", .finishRequest 500)
  , ("retireGeneration", .retireGeneration 1)
  ]

def runtimeReconcileMachine : StateMachineContract :=
  machineContract
    "RuntimeReconcile"
    runtimeReconcileStateNames
    []
    (actionNames runtimeReconcileActions)
    (transitionPairsFromSamples
      runtimeReconcileSamples
      runtimeReconcileActions
      RuntimeState.step?
      (fun state => state.phase.toDefraDB))

def sessionRecoveryLegalTransitions : List TransitionPair :=
  sessionRecoveryCases.filterMap fun witness =>
    if witness.legal then
      some { source := witness.preLatestState, target := witness.postLatestState }
    else
      none

def sessionRecoveryMachine : StateMachineContract :=
  machineContract
    "SessionRecovery"
    [RequestState.failed.toDefraDB, RequestState.pending.toDefraDB]
    []
    ["reissueFailed"]
    sessionRecoveryLegalTransitions

def inferenceCallStates : List InferenceCallState :=
  [ .queued, .running, .cancelled, .completed, .failed ]

def inferenceCallStateNames : List String :=
  inferenceCallStates.map InferenceCallState.toDefraDB

def inferenceCallActions : List (String × InferenceCall.Action) :=
  [ ("start", .start)
  , ("complete", .complete)
  , ("fail", .fail)
  , ("cancel", .cancel)
  ]

def inferenceCallWithState (state : InferenceCallState) : InferenceCall :=
  { callId := 1
  , requestId := 1
  , backend := contractBackend
  , state := state
  }

def inferenceCallMachine : StateMachineContract :=
  machineContract
    "InferenceCall"
    inferenceCallStateNames
    (terminalNames inferenceCallStates InferenceCallState.toDefraDB)
    (actionNames inferenceCallActions)
    (transitionPairsFromSamples
      (inferenceCallStates.map inferenceCallWithState)
      inferenceCallActions
      InferenceCall.step?
      (fun call => call.state.toDefraDB))

def toolRetryDispositions : List ToolExecution.RetryDisposition :=
  ToolExecution.RetryDisposition.all

def toolRetryDispositionNames : List String :=
  toolRetryDispositions.map ToolExecution.RetryDisposition.toDefraDB

def vocabularies : List VocabularyContract :=
  [ { domain := "RequestState", values := requestStateNames }
  , { domain := "ExecutionOrigin", values :=
        [.interactive, .scheduled].map ExecutionOrigin.toDefraDB }
  , { domain := "ProcessState", values := processStateNames }
  , { domain := "PersistenceState", values := persistenceStateNames }
  , { domain := "PersistenceFailurePolicy", values :=
        [.failOpen, .failClosed].map PersistenceState.FailurePolicy.toDefraDB }
  , { domain := "ReconcilePhase", values := runtimeReconcileStateNames }
  , { domain := "StorageObservation", values := storageObservationStateNames }
  , { domain := "SessionRecoveryLatestRequestState"
    , values := [RequestState.failed.toDefraDB, RequestState.pending.toDefraDB]
    }
  , { domain := "InferenceCallState", values := inferenceCallStateNames }
  , { domain := "InferenceCallTerminalReason", values :=
        [ .cancelled
        , .backendGone
        , .queueFull
        , .streamDroppedBeforeTerminalResponse
        ].map InferenceCallTerminalReason.toDefraDB
    }
  , { domain := "ToolRetryDisposition", values := toolRetryDispositionNames }
  ]

def stateMachines : List StateMachineContract :=
  [ requestMachine
  , processMachine
  , persistenceMachine "Persistence.failClosed" .failClosed
  , persistenceMachine "Persistence.failOpen" .failOpen
  , storageObservationMachine "StorageObservation.failClosed" .failClosed
  , storageObservationMachine "StorageObservation.failOpen" .failOpen
  , runtimeReconcileMachine
  , sessionRecoveryMachine
  , inferenceCallMachine
  ]

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
    ++ "\"post_latest_state\":" ++ jsonString witness.postLatestState ++ ","
    ++ "\"pre_latest_admission\":" ++ jsonString witness.preLatestAdmission ++ ","
    ++ "\"post_latest_admission\":" ++ jsonString witness.postLatestAdmission ++ ","
    ++ "\"pre_failed_admission\":" ++ jsonString witness.preFailedAdmission ++ ","
    ++ "\"post_failed_admission\":" ++ jsonString witness.postFailedAdmission ++ ","
    ++ "\"post_new_admission\":" ++ jsonString witness.postNewAdmission ++ ","
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
    ++ "\"pre_new_request_exists\":" ++ boolString witness.preNewRequestExists ++ ","
    ++ "\"old_request_retained\":" ++ boolString witness.oldRequestRetained ++ ","
    ++ "\"new_request_inserted\":" ++ boolString witness.newRequestInserted ++ ","
    ++ "\"origin_preserved\":" ++ boolString witness.originPreserved ++ ","
    ++ "\"backend_preserved\":" ++ boolString witness.backendPreserved
    ++ "}"

def snapshotJson : String :=
  "{"
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ "\"vocabularies\":"
      ++ jsonArray (vocabularies.map VocabularyContract.toJson) ++ ","
    ++ "\"state_machines\":"
      ++ jsonArray (stateMachines.map StateMachineContract.toJson) ++ ","
    ++ "\"trigger_dispatch_case_count\":"
      ++ toString Conformance.TriggerContracts.triggerDispatchCaseCount ++ ","
    ++ "\"trigger_dispatch_cases\":"
      ++ Conformance.TriggerContracts.triggerDispatchCasesJson ++ ","
    ++ "\"runtime_reconcile_cases\":"
      ++ jsonArray (runtimeReconcileCases.map runtimeReconcileCaseJson) ++ ","
    ++ "\"session_recovery_cases\":"
      ++ jsonArray (sessionRecoveryCases.map sessionRecoveryCaseJson) ++ ","
    ++ "\"follow_up_hooks\":["
      ++ jsonString "ToolExecution idempotent MCP call retry contract"
      ++ "],"
    ++ "\"coverage_ledger\":"
      ++ coverageLedgerJson
    ++ "}"

def contractJsonBegin : String :=
  "---BEGIN DEFRA LEAN CONTRACT JSON---"

def contractJsonEnd : String :=
  "---END DEFRA LEAN CONTRACT JSON---"

def main : IO Unit := do
  IO.println contractJsonBegin
  IO.println snapshotJson
  IO.println contractJsonEnd

end Conformance.Contracts

def main : IO Unit :=
  Conformance.Contracts.main
