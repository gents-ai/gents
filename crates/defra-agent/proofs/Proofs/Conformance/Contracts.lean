import Proofs.Request
import Proofs.Process
import Proofs.Persistence
import Proofs.SessionRecovery
import Proofs.InferenceCall
import Proofs.Conformance.Triggers.Contracts

/-!
# Rust Conformance Contracts

This module is the Lean-owned extraction surface for Rust conformance tests.
Rust runs this file with `lake env lean --run` and consumes the JSON emitted by
`main`. State vocabularies and transition tables are evaluated from the Lean
constructors, `toDefraDB` functions, executable `step?` functions, and finite
witness contexts below.

`RuntimeReconcile` is intentionally exposed only as a follow-up hook here so
this extraction stays scoped to the initial executable domains below. Add it as
another `StateMachineContract` when the runtime-reconcile contract is ready to
join the Rust conformance gate.
-/

namespace Conformance.Contracts

structure TransitionPair where
  source : String
  target : String
  deriving DecidableEq, Repr

structure VocabularyContract where
  domain : String
  values : List String
  deriving Repr

structure StateMachineContract where
  domain : String
  states : List String
  stateCount : Nat
  terminalStates : List String
  nonterminalStates : List String
  actions : List String
  legalTransitions : List TransitionPair
  illegalTransitions : List TransitionPair
  deriving Repr

def jsonString (s : String) : String :=
  "\"" ++ s ++ "\""

def jsonArray (values : List String) : String :=
  "[" ++ String.intercalate "," values ++ "]"

def jsonStringArray (values : List String) : String :=
  jsonArray (values.map jsonString)

def dedup {α : Type} [DecidableEq α] (values : List α) : List α :=
  values.foldl
    (fun seen value => if value ∈ seen then seen else seen ++ [value])
    []

def without {α : Type} [DecidableEq α] (values excluded : List α) : List α :=
  values.filter fun value => if value ∈ excluded then false else true

def allPairs (states : List String) : List TransitionPair :=
  states.flatMap fun source =>
    states.map fun target => { source := source, target := target }

def illegalPairs (states : List String) (legal : List TransitionPair) : List TransitionPair :=
  without (allPairs states) legal

def terminalNames {α : Type} [HasTerminal α]
    (states : List α)
    (name : α → String) : List String :=
  states.filterMap fun state =>
    if isTerminal state then some (name state) else none

def actionNames {α : Type} (actions : List (String × α)) : List String :=
  actions.map Prod.fst

def transitionPairsFromSamples {σ α : Type}
    (samples : List σ)
    (actions : List (String × α))
    (step : σ → α → Option σ)
    (stateName : σ → String) : List TransitionPair :=
  dedup <|
    samples.flatMap fun pre =>
      actions.filterMap fun action =>
        match step pre action.snd with
        | some post => some { source := stateName pre, target := stateName post }
        | none => none

def machineContract
    (domain : String)
    (states terminalStates actions : List String)
    (legalTransitions : List TransitionPair) : StateMachineContract :=
  let legalTransitions := dedup legalTransitions
  { domain := domain
  , states := states
  , stateCount := states.length
  , terminalStates := terminalStates
  , nonterminalStates := without states terminalStates
  , actions := actions
  , legalTransitions := legalTransitions
  , illegalTransitions := illegalPairs states legalTransitions
  }

def requestStates : List RequestState :=
  [ .pending
  , .claimed
  , .processing
  , .inputRequired
  , .completed
  , .failed
  , .superseded
  , .dead
  , .interrupted
  ]

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

def sessionRecoveryFailedId : RequestId := 1

def sessionRecoveryNewId : RequestId := 2

def sessionRecoveryFailedContext : RequestContext :=
  requestContext .failed .released

def sessionRecoveryPre : SessionState :=
  { sessionId := 1
  , behaviorId := 1
  , requestIds := {sessionRecoveryFailedId}
  , ctx := fun rid =>
      if rid = sessionRecoveryFailedId then
        sessionRecoveryFailedContext
      else
        requestContext .pending .released
  , latest := sessionRecoveryFailedId
  }

def sessionRecoveryLegalTransitions : List TransitionPair :=
  match SessionState.step?
      sessionRecoveryPre
      (.reissueFailed sessionRecoveryFailedId sessionRecoveryNewId) with
  | some post =>
      [ { source := (sessionRecoveryPre.ctx sessionRecoveryPre.latest).state.toDefraDB
        , target := (post.ctx post.latest).state.toDefraDB
        }
      ]
  | none => []

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

def vocabularies : List VocabularyContract :=
  [ { domain := "RequestState", values := requestStateNames }
  , { domain := "ExecutionOrigin", values :=
        [.interactive, .scheduled].map ExecutionOrigin.toDefraDB }
  , { domain := "ProcessState", values := processStateNames }
  , { domain := "PersistenceState", values := persistenceStateNames }
  , { domain := "PersistenceFailurePolicy", values :=
        [.failOpen, .failClosed].map PersistenceState.FailurePolicy.toDefraDB }
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
  ]

def stateMachines : List StateMachineContract :=
  [ requestMachine
  , processMachine
  , persistenceMachine "Persistence.failClosed" .failClosed
  , persistenceMachine "Persistence.failOpen" .failOpen
  , sessionRecoveryMachine
  , inferenceCallMachine
  ]

def TransitionPair.toJson (pair : TransitionPair) : String :=
  "{"
    ++ "\"from\":" ++ jsonString pair.source ++ ","
    ++ "\"to\":" ++ jsonString pair.target
    ++ "}"

def VocabularyContract.toJson (contract : VocabularyContract) : String :=
  "{"
    ++ "\"domain\":" ++ jsonString contract.domain ++ ","
    ++ "\"values\":" ++ jsonStringArray contract.values
    ++ "}"

def StateMachineContract.toJson (contract : StateMachineContract) : String :=
  "{"
    ++ "\"domain\":" ++ jsonString contract.domain ++ ","
    ++ "\"states\":" ++ jsonStringArray contract.states ++ ","
    ++ "\"state_count\":" ++ toString contract.stateCount ++ ","
    ++ "\"terminal_states\":" ++ jsonStringArray contract.terminalStates ++ ","
    ++ "\"nonterminal_states\":" ++ jsonStringArray contract.nonterminalStates ++ ","
    ++ "\"actions\":" ++ jsonStringArray contract.actions ++ ","
    ++ "\"legal_transitions\":"
      ++ jsonArray (contract.legalTransitions.map TransitionPair.toJson) ++ ","
    ++ "\"illegal_transitions\":"
      ++ jsonArray (contract.illegalTransitions.map TransitionPair.toJson)
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
    ++ "\"follow_up_hooks\":["
      ++ jsonString "RuntimeReconcile executable state machine contract"
      ++ "]"
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
