import Proofs.Request
import Proofs.Process
import Proofs.Persistence
import Proofs.StorageObservation
import Proofs.SessionRecovery
import Proofs.InferenceCall
import Proofs.Conformance.ContractTypes
import Proofs.RuntimeReconcile
import Proofs.Conformance.ContractCases
import Proofs.ToolExecution

/-!
# Core Conformance Vocabularies and State Machines

Lean-owned vocabulary lists and transition tables emitted by
`Proofs.Conformance.Contracts`.
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
    requestStateNames
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
    , values := requestStateNames
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

end Conformance.Contracts
