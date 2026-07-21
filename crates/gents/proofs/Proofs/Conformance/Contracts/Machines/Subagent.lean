import Proofs.ToolExecution
import Proofs.Conformance.ContractTypes

/-!
# Subagent Tool Vocabularies

Vocabulary-only and projection contracts adjacent to the ToolCall machine.
-/

namespace Conformance.Contracts

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

end Conformance.Contracts
