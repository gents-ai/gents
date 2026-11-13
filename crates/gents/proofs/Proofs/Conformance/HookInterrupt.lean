import Proofs.ToolCalls.Background.Interrupt
import Proofs.Conformance.Family

namespace Conformance.HookInterrupt

open Lean ToolExecution Background.Interrupt

/-- Two hooks on one node, each bound to its own session and request, holding
`ownCalls` and `otherCalls` running foreground calls; the first hook is
interrupted. -/
structure Hooks where
  ownCalls : Nat
  otherCalls : Nat
  deriving ToJson

structure Interrupted where
  cancelled : Nat
  ownStates : List String
  otherStates : List String
  deriving ToJson

private def running (requestId : Nat) (callId : Nat) : ToolCallContext :=
  { callId, requestId, state := .running, operation := .nativeCommand, deadline := 100
  , startedAt := some 1, currentTime := 10, persistence := .committed, awaitMode := .foreground }

@[conformance_family]
def scope : Conformance.Family Hooks Interrupted where
  inputs := [0, 1, 2].flatMap fun ownCalls => [0, 1, 2].map fun otherCalls => { ownCalls, otherCalls }
  model := fun h =>
    let (after, cancelled) := interruptHook
      { own := (List.range h.ownCalls).map (running 1), other := (List.range h.otherCalls).map (running 2) }
    { cancelled
    , ownStates := after.own.map (·.state.toDefraDB)
    , otherStates := after.other.map (·.state.toDefraDB) }

end Conformance.HookInterrupt
