import Proofs.ToolExecution
import Proofs.Conformance.ContractTypes

/-!
# Tool Call Conformance Machine
-/

namespace Conformance.Contracts

def toolCallStates : List ToolExecution.ToolCallState :=
  ToolExecution.ToolCallState.all

def toolCallStateNames : List String :=
  toolCallStates.map ToolExecution.ToolCallState.toDefraDB

def toolCallCancelCauses : List ToolExecution.CancelCause :=
  ToolExecution.CancelCause.all

def toolCallCancelCauseNames : List String :=
  toolCallCancelCauses.map ToolExecution.CancelCause.toDefraDB

def toolRetryDispositions : List ToolExecution.RetryDisposition :=
  ToolExecution.RetryDisposition.all

def toolRetryDispositionNames : List String :=
  toolRetryDispositions.map ToolExecution.RetryDisposition.toDefraDB

def failureClassNames : List String :=
  ToolExecution.FailureClass.all.map ToolExecution.FailureClass.toDefraDB

def toolCallCancelActions : List (String × ToolExecution.ToolCallContext.Action) :=
  toolCallCancelCauses.flatMap fun cause =>
    [ ("cancelBeforeDispatch_" ++ cause.toDefraDB, .cancelBeforeDispatch cause)
    , ("cancelDuringRun_" ++ cause.toDefraDB, .cancelDuringRun cause)
    ]

def toolCallActions : List (String × ToolExecution.ToolCallContext.Action) :=
  [ ("dispatch", .dispatch)
  , ("spawnFailed_external", .spawnFailed .external)
  , ("complete", .complete)
  , ("fail_external", .fail .external)
  , ("timeout", .timeout)
  , ("background", .background)
  , ("foreground", .foreground)
  ] ++ toolCallCancelActions

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

/-- Mode evidence for the executable `foreground` arm. The ordinary running
sample starts in foreground and already exercises `background`; this sample
closes the inverse mode-flip row. -/
def toolCallModeSamples : List ToolExecution.ToolCallContext :=
  [ { toolCallWithState .running with awaitMode := .background } ]

/-- Named transition rows for the ToolCall machine. Mode flips are
state-preserving on `ToolCallState`, so the pair-based `legalTransitions` list
cannot express them. A `create_session`/`send_message` row closes through the
same `complete`/`fail` edges as every other row; there are no bridge edges. -/
def toolCallNamedTransitions : List NamedTransition :=
  [ { name := "complete"
    , source := "running"
    , target := "completed" }
  , { name := "fail"
    , source := "running"
    , target := "failed" }
  , { name := "background"
    , source := "running"
    , target := "running" }
  , { name := "foreground"
    , source := "running"
    , target := "running" }
  ]

def toolCallMachine : StateMachineContract :=
  let base :=
    machineContract
      "ToolCall"
      toolCallStateNames
      (terminalNames toolCallStates ToolExecution.ToolCallState.toDefraDB)
      (actionNames toolCallActions)
      (transitionPairsFromSamples
        (toolCallStates.map toolCallWithState ++ toolCallModeSamples)
        toolCallActions
        ToolExecution.ToolCallContext.step?
        (fun call => call.state.toDefraDB))
  { base with namedTransitions := toolCallNamedTransitions }

end Conformance.Contracts
