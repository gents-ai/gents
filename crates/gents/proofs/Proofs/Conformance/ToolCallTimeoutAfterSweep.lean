import Proofs.ToolCalls.Recovery.Sweeps.ToolCalls
import Proofs.Conformance.Family

namespace Conformance.ToolCallTimeoutAfterSweep

open Lean Recovery ToolExecution

def runningForeground : ToolCallContext :=
  { callId := 77
  , requestId := 900
  , state := .running
  , operation := .nativeCommand
  , deadline := 100
  , startedAt := some 1
  , currentTime := 10
  , failureClass := none
  , persistence := .committed
  , awaitMode := .foreground
  }

structure Input where
  parent : String
  deriving ToJson

structure Observed where
  swept : Nat
  timeoutReported : Bool
  lifecycleState : String
  cancelCause : Option String
  failureClass : Option String
  deriving ToJson

def rowOf (i : Input) : TerminalParentToolRow :=
  { call := runningForeground
  , parentTerminal := i.parent != "live"
  , parentInterrupted := i.parent == "interrupted"
  }

@[conformance_family]
def adoption : Family Input Observed where
  inputs := [{ parent := "live" }, { parent := "interrupted" }, { parent := "failed" }]
  model := fun i =>
    let row := rowOf i
    { swept := terminalParentToolMeasure row
      timeoutReported := deadlineActionTimesOut row
      lifecycleState := (timeoutAfterSweepState row).toDefraDB
      cancelCause :=
        if deadlineActionTimesOut row then some "deadline"
        else sweptCancelCause (sweptCause row)
      failureClass :=
        if deadlineActionTimesOut row then some "external"
        else sweptFailureClass (sweptCause row) }

end Conformance.ToolCallTimeoutAfterSweep
