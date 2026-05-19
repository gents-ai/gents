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

end Conformance.Contracts
