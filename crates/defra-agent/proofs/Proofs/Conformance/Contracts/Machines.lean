import Proofs.Request
import Proofs.Process
import Proofs.Persistence
import Proofs.StorageObservation
import Proofs.SessionRecovery
import Proofs.InferenceCall
import Proofs.Conformance.ContractTypes
import Proofs.RuntimeReconcile
import Proofs.PairingReconcile
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

def pairingReconcileStates : List PairingReconcile.PairingPhase :=
  [ .idle, .diverged, .converged, .crashed ]

def pairingReconcileStateNames : List String :=
  pairingReconcileStates.map PairingReconcile.PairingPhase.toContract

def pairingReconcileActions : List (String × PairingReconcile.TransitionKind) :=
  [ ("operatorWrite", .operatorWrite)
  , ("reconcileInstall", .reconcileInstall)
  , ("reconcileTeardown", .reconcileTeardown)
  , ("crash", .crash)
  ]

def pairingReconcileMachine : StateMachineContract :=
  machineContract
    "PairingReconcile"
    pairingReconcileStateNames
    []
    (actionNames pairingReconcileActions)
    (transitionPairsFromSamples
      pairingReconcileStates
      pairingReconcileActions
      PairingReconcile.step?
      PairingReconcile.PairingPhase.toContract)

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

def toolCallStates : List ToolExecution.ToolCallState :=
  ToolExecution.ToolCallState.all

def toolCallStateNames : List String :=
  toolCallStates.map ToolExecution.ToolCallState.toDefraDB

def toolCallActions : List (String × ToolExecution.ToolCallContext.Action) :=
  [ ("dispatch", .dispatch)
  , ("spawnFailed_external", .spawnFailed .external)
  , ("complete", .complete)
  , ("fail_external", .fail .external)
  , ("timeout", .timeout)
  , ("cancelBeforeDispatch", .cancelBeforeDispatch)
  , ("cancelDuringRun", .cancelDuringRun)
  ]

def toolCallWithState (state : ToolExecution.ToolCallState) : ToolExecution.ToolCallContext :=
  { callId := 1
  , requestId := 1
  , state := state
  , operation := .nativeCommand
  , deadline := 1
  , startedAt := none
  , currentTime := 2
  , failureClass := none
  , persistence := .committed
  }

/-- Named transition rows for the ToolCall machine.

Bucket 2 of the R2 Rust subagent data plane consumes these to assert that
the Rust runtime's transition matrix matches Lean. They cover three new
classes of edge that the plain `(source, target)` pairs in
`legalTransitions` cannot express on their own:

* native-only edges: `complete` and `fail` on a tool whose
  `childRequestId = none`. The relational `Transition.complete` constructor
  carries `pre.childRequestId = none` as a precondition (and `step?` mirrors
  it); `requires_native: true` lets the Rust matrix test reject calling
  these on a subagent-typed tool.
* mode-flip edges: `background`, `foreground`, `detach_running`,
  `detach_pending` are state-preserving on `ToolCallState` and so don't
  appear in the pair-based `legalTransitions` list. They live in
  `ToolCallContext.Transition` (subagent extensions in `State.lean`) and
  flip `awaitMode`/`cancelPolicy` while leaving `state` unchanged.
  `detach` is split into two rows (`detach_running`, `detach_pending`)
  mirroring the `bridge_failure` split pattern, because its
  `h_live` precondition permits both `.pending` and `.running`.
* bridge edges: `bridge_complete`, `bridge_failure`,
  `bridge_cancel_cascade`. These are defined relationally on
  `Subagent.BridgedState.Transition`, not on `ToolCallContext.Transition`,
  but their effect on the bridge tool's inner state is what Bucket 2 needs
  to enforce in Rust. `bridge_complete` advances the bridge tool from
  `running → completed` (with `requires_child = true`); `bridge_failure`
  drives `running → failed` or `running → cancelled` (per the disjunction in
  `BridgedState.Transition.bridge_failure`); `bridge_cancel_cascade` is
  state-preserving on the parent's bridge tool (it sets the child's
  `interruptRequestedAt`) so its row uses `running → running`. -/
def toolCallNamedTransitions : List NamedTransition :=
  [ -- native-only inner transitions: subagent-typed tools (with a child) take
    -- the bridge_* path instead.
    { name := "complete_native"
    , source := "running"
    , target := "completed"
    , requiresNative := true }
  , { name := "fail_native"
    , source := "running"
    , target := "failed"
    , requiresNative := true }
    -- mode flips (state-preserving on ToolCallState):
  , { name := "background"
    , source := "running"
    , target := "running" }
  , { name := "foreground"
    , source := "running"
    , target := "running" }
  , { name := "detach_running"
    , source := "running"
    , target := "running" }
  , { name := "detach_pending"
    , source := "pending"
    , target := "pending" }
    -- bridge edges (subagent-typed tools only):
  , { name := "bridge_complete"
    , source := "running"
    , target := "completed"
    , requiresChild := true }
  , { name := "bridge_failure_failed"
    , source := "running"
    , target := "failed"
    , requiresChild := true }
  , { name := "bridge_failure_cancelled"
    , source := "running"
    , target := "cancelled"
    , requiresChild := true }
  , { name := "bridge_cancel_cascade"
    , source := "running"
    , target := "running"
    , requiresChild := true }
  ]

def toolCallMachine : StateMachineContract :=
  let base :=
    machineContract
      "ToolCall"
      toolCallStateNames
      (terminalNames toolCallStates ToolExecution.ToolCallState.toDefraDB)
      (actionNames toolCallActions)
      (transitionPairsFromSamples
        (toolCallStates.map toolCallWithState)
        toolCallActions
        ToolExecution.ToolCallContext.step?
        (fun call => call.state.toDefraDB))
  { base with namedTransitions := toolCallNamedTransitions }

/-- AwaitMode is a static enum on `ToolCallContext` (foreground/background).
    It has no transitions in its own right — the mode-flip edges live on
    `toolCallMachine`'s `namedTransitions` (`background`, `foreground`).
    Emitted as a vocabulary-only state machine so Bucket 1 (vocabulary
    round-trip) can target it the same way it targets `ToolCallState`. -/
def awaitModeMachine : StateMachineContract :=
  let names := Subagent.AwaitMode.all.map Subagent.AwaitMode.toDefraDB
  machineContract
    "AwaitMode"
    names
    []        -- no terminal states; modes are not lifecycle states
    []        -- no actions
    []        -- no transitions

/-- CancelPolicy is a static enum on `ToolCallContext` (cascade/detach).
    Only the cascade → detach edge is allowed at runtime, surfaced as
    `toolCallMachine`'s `detach` named transition. Emitted vocabulary-only
    here. -/
def cancelPolicyMachine : StateMachineContract :=
  let names := Subagent.CancelPolicy.all.map Subagent.CancelPolicy.toDefraDB
  machineContract
    "CancelPolicy"
    names
    []
    []
    []

/-- Projection from a child's terminal `RequestState` to the parent
    bridge tool's `ToolCallState` under `bridge_complete` / `bridge_failure`.

The `namedTransitions` here encode the projection rule that R2 Bucket 2
asserts against the Rust runtime: when a child request reaches a terminal
state, the parent's bridge tool is driven to the projected tool state.

  * `completed` is intentionally absent from the source vocabulary because
    the `completed → completed` edge is handled by the dedicated
    `bridge_complete` constructor, which has stricter preconditions
    (`pre.persistence = .committed`). Including it here would conflate
    success-path persistence with failure-path projection.
  * `interrupted` projects to `cancelled` (operator-driven cancel); all
    other terminals project to `failed`. Matches `BridgedState.Transition.
    bridge_failure`'s `tPost.state = .failed ∨ tPost.state = .cancelled`.

`legalTransitions` is left empty: source and target live in different
vocabularies (child `RequestState` → parent `ToolCallState`), so the
pair-based legal/illegal split would be misleading. The projection lives
in `namedTransitions`, where `source` is documented as a child terminal
and `target` as the projected tool state. -/
def childTerminalMachine : StateMachineContract :=
  let base :=
    machineContract
      "ChildTerminal"
      ["failed", "dead", "interrupted", "superseded"]
      ["failed", "dead", "interrupted", "superseded"]  -- every source row is a terminal child state
      []  -- projection has no actions; rule is purely structural
      []  -- pair-based legal transitions intentionally empty: cross-vocabulary edges are in namedTransitions
  { base with
      namedTransitions :=
        [ { name := "project_failed"
          , source := "failed"
          , target := "failed" }
        , { name := "project_dead"
          , source := "dead"
          , target := "failed" }
        , { name := "project_interrupted"
          , source := "interrupted"
          , target := "cancelled" }
        , { name := "project_superseded"
          , source := "superseded"
          , target := "failed" }
        ] }

def toolRetryDispositions : List ToolExecution.RetryDisposition :=
  ToolExecution.RetryDisposition.all

def toolRetryDispositionNames : List String :=
  toolRetryDispositions.map ToolExecution.RetryDisposition.toDefraDB

def failureClassNames : List String :=
  ToolExecution.FailureClass.all.map ToolExecution.FailureClass.toDefraDB

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
  , { domain := "ToolCallState", values := toolCallStateNames }
  , { domain := "ToolFailureClass", values := failureClassNames }
  , { domain := "ToolRetryDisposition", values := toolRetryDispositionNames }
  , { domain := "AwaitMode"
    , values := Subagent.AwaitMode.all.map Subagent.AwaitMode.toDefraDB
    }
  , { domain := "CancelPolicy"
    , values := Subagent.CancelPolicy.all.map Subagent.CancelPolicy.toDefraDB
    }
  , { domain := "ChildTerminal"
    , values := ["failed", "dead", "interrupted", "superseded"]
    }
  ]

def stateMachines : List StateMachineContract :=
  [ requestMachine
  , processMachine
  , persistenceMachine "Persistence.failClosed" .failClosed
  , persistenceMachine "Persistence.failOpen" .failOpen
  , storageObservationMachine "StorageObservation.failClosed" .failClosed
  , storageObservationMachine "StorageObservation.failOpen" .failOpen
  , runtimeReconcileMachine
  , pairingReconcileMachine
  , sessionRecoveryMachine
  , inferenceCallMachine
  , toolCallMachine
  , awaitModeMachine
  , cancelPolicyMachine
  , childTerminalMachine
  ]

end Conformance.Contracts
