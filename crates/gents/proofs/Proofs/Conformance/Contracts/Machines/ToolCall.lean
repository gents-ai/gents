import Proofs.ToolExecution
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def toolCallStates : List ToolExecution.ToolCallState :=
  ToolExecution.ToolCallState.all

def toolCallStateNames : List String :=
  toolCallStates.map ToolExecution.ToolCallState.toDefraDB

def toolCallCancelCauses : List ToolExecution.CancelCause :=
  ToolExecution.CancelCause.all

def toolCallCancelCauseNames : List String :=
  toolCallCancelCauses.map ToolExecution.CancelCause.toDefraDB

def toolCallCancelActions : List (String × ToolExecution.ToolCallContext.Action) :=
  toolCallCancelCauses.flatMap fun cause =>
    [ ("cancelBeforeDispatch_" ++ cause.toDefraDB, .cancelBeforeDispatch cause)
    , ("cancelDuringRun_" ++ cause.toDefraDB, .cancelDuringRun cause)
    , ("cancelWhileHeld_" ++ cause.toDefraDB, .cancelWhileHeld cause)
    ]

def toolCallActions : List (String × ToolExecution.ToolCallContext.Action) :=
  [ ("dispatch", .dispatch)
  , ("spawnFailed_external", .spawnFailed .external)
  , ("complete", .complete)
  , ("fail_external", .fail .external)
  , ("timeout", .timeout)
  , ("holdForApproval", .holdForApproval)
  , ("recordApproval_approved", .recordApproval .approved)
  , ("recordApproval_denied", .recordApproval .denied)
  , ("approve", .approve)
  , ("deny", .deny)
  , ("timeoutWhileHeld", .timeoutWhileHeld)
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

def toolCallApprovalSamples : List ToolExecution.ToolCallContext :=
  [ { toolCallWithState .awaitingApproval with approval := some .approved }
  , { toolCallWithState .awaitingApproval with approval := some .denied }
  ]

def toolCallNamedTransitions : List NamedTransition :=
  [
    { name := "complete_native"
    , source := "running"
    , target := "completed"
    , requiresNative := true }
  , { name := "fail_native"
    , source := "running"
    , target := "failed"
    , requiresNative := true }
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
        (toolCallStates.map toolCallWithState ++ toolCallApprovalSamples)
        toolCallActions
        ToolExecution.ToolCallContext.step?
        (fun call => call.state.toDefraDB))
  { base with namedTransitions := toolCallNamedTransitions }

end Conformance.Contracts
