import Proofs.ToolCalls.Recovery.Sweeps.CallerInterrupt
import Proofs.Conformance.Family

namespace Conformance.PluginJobCallerInterrupt

open Lean ToolExecution

structure CallerInterruptInput where
  name : String
  deriving ToJson

structure CallerInterruptOutput where
  toolCallsTerminalized : Nat
  rowRunning : Bool
  /-- The sweep interrupts a row's live children only when it terminalizes the
  row; this row is not terminalized, so its child is left alone. -/
  childInterrupted : Bool
  deriving ToJson

/-- A running background plugin job with a live child, the situation the
caller interrupt and the orphan sweep are run on. -/
def runningPluginJob : ToolCallContext :=
  { callId := 1
  , requestId := 900
  , state := .running
  , operation := .pluginJob
  , deadline := 100
  , startedAt := some 1
  , currentTime := 10
  , persistence := .committed
  , awaitMode := .background
  }

@[conformance_family]
def sweep : Family CallerInterruptInput CallerInterruptOutput where
  inputs := [{ name := "plugin_job_caller_interrupted" }]
  model := fun _ =>
    { toolCallsTerminalized :=
        if Recovery.orphanSweepTerminalizes runningPluginJob then 1 else 0
      rowRunning :=
        !Recovery.orphanSweepTerminalizes runningPluginJob &&
          decide ((Background.Interrupt.interruptTool runningPluginJob).state = .running)
      childInterrupted := Recovery.orphanSweepTerminalizes runningPluginJob }

end Conformance.PluginJobCallerInterrupt
