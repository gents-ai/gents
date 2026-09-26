import Proofs.Background.Interrupt
import Proofs.Conformance.ContractCases.Types

/-!
# Interrupt Disposition Conformance Cases

Every owned tool-call shape (lifecycle state × await mode × child link ×
cancellation policy) with the disposition and post-state computed by running
`Subagent.Interrupt.interruptTool`. The Rust consumer compares its interrupt
owner against every row and drives the running rows through the hook's
in-flight interrupt path.
-/

namespace Conformance.ContractCases

open Subagent

def interruptDispositionTool
    (state : ToolExecution.ToolCallState) (mode : AwaitMode)
    (childLinked : Bool) (policy : CancelPolicy) :
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
  , cancelPolicy := policy
  , childRequestId := if childLinked then some 901 else none
  }

def interruptDispositionCase
    (state : ToolExecution.ToolCallState) (mode : AwaitMode)
    (childLinked : Bool) (policy : CancelPolicy) : InterruptDispositionCase :=
  let tool := interruptDispositionTool state mode childLinked policy
  let post := Interrupt.interruptTool tool
  { name := s!"interrupt_{state.toDefraDB}_{mode.toDefraDB}_" ++
      (if childLinked then "bridge" else "native") ++ s!"_{policy.toDefraDB}"
  , state := state.toDefraDB
  , awaitMode := mode.toDefraDB
  , childLinked := childLinked
  , cancelPolicy := policy.toDefraDB
  , disposition := (Interrupt.disposition tool).toContract
  , postState := post.state.toDefraDB
  , postAwaitMode := post.awaitMode.toDefraDB
  }

def interruptDispositionCases : List InterruptDispositionCase :=
  ToolExecution.ToolCallState.all.flatMap fun state =>
    AwaitMode.all.flatMap fun mode =>
      [false, true].flatMap fun childLinked =>
        CancelPolicy.all.map fun policy =>
          interruptDispositionCase state mode childLinked policy

theorem interruptDispositionCases_complete :
    interruptDispositionCases.length = 48 := by
  native_decide

/-- No emitted row cancels running background work or a running subagent. -/
theorem interruptDispositionCases_preserve_running_background_and_subagents :
    ∀ witness ∈ interruptDispositionCases,
      witness.state = "running" →
      (witness.awaitMode = "background" ∨ witness.childLinked = true) →
        witness.postState = "running" := by
  native_decide

end Conformance.ContractCases
