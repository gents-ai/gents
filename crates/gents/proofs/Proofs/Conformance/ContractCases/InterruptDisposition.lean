import Proofs.Background.Interrupt
import Proofs.Conformance.ContractCases.Types

/-!
# Interrupt Disposition Conformance Cases

Every owned tool-call shape (lifecycle state × await mode) with the
disposition and post-state computed by running
`Background.Interrupt.interruptTool`. The Rust consumer compares its interrupt
owner against every row and drives the running rows through the hook's
in-flight interrupt path.
-/

namespace Conformance.ContractCases

open Background

def interruptDispositionTool
    (state : ToolExecution.ToolCallState) (mode : ToolExecution.AwaitMode) :
    ToolExecution.ToolCallContext :=
  { callId := 1
  , requestId := 900
  , state := state
  , operation := .nativeCommand
  , deadline := 100
  , startedAt := some 1
  , currentTime := 10
  , persistence := .committed
  , awaitMode := mode
  }

def interruptDispositionCase
    (state : ToolExecution.ToolCallState) (mode : ToolExecution.AwaitMode) :
    InterruptDispositionCase :=
  let tool := interruptDispositionTool state mode
  let post := Interrupt.interruptTool tool
  { name := s!"interrupt_{state.toDefraDB}_{mode.toDefraDB}"
  , state := state.toDefraDB
  , awaitMode := mode.toDefraDB
  , disposition := (Interrupt.disposition tool).toContract
  , postState := post.state.toDefraDB
  , postAwaitMode := post.awaitMode.toDefraDB
  }

def interruptDispositionCases : List InterruptDispositionCase :=
  ToolExecution.ToolCallState.all.flatMap fun state =>
    ToolExecution.AwaitMode.all.map fun mode =>
      interruptDispositionCase state mode

theorem interruptDispositionCases_complete :
    interruptDispositionCases.length = 12 := by
  native_decide

/-- No emitted row cancels running background work. -/
theorem interruptDispositionCases_preserve_running_background :
    ∀ witness ∈ interruptDispositionCases,
      witness.state = "running" → witness.awaitMode = "background" →
        witness.postState = "running" := by
  native_decide

end Conformance.ContractCases
