import Proofs.Workflow.FanOut

/-!
# Workflow conformance witnesses

Finite witness rows for the Rust barrier-projection fence.

Every `legal` annotation below is *entailed by the model*, not hand-asserted:
`workflowCasesLegalCorrect` decides that each row's `legal` equals the computable
barrier predicate `barrierLegal` applied to that row's fields. A wrong annotation
fails to build.

`barrierLegal` is the computable mirror of the Rust
`workflow_barrier_projection_legal`: a group is legal iff it is non-empty AND
(synthesis is absent OR every fan-out bridge is terminal).
-/

namespace Workflow
namespace Conformance

open ToolExecution

structure BarrierCase where
  name : String
  groupTerminalStates : List ToolCallState
  synthesisPresent : Bool
  legal : Bool

/-- Computable barrier-legality predicate, definitionally aligned with the Rust
    `workflow_barrier_projection_legal(states, synthesis_present)`:
    `non-empty ∧ (¬synthesis_present ∨ states.all isTerminal)`.

    The `states.all isTerminal` clause is exactly `WorkflowGroup.allTerminalB`'s
    body; the terminal set is `{completed, failed, timedOut, cancelled}` via the
    `HasTerminal ToolCallState` instance, matching
    `WORKFLOW_TERMINAL_TOOL_STATES` on the Rust side. -/
def barrierLegal (states : List ToolCallState) (synthesisPresent : Bool) : Bool :=
  !states.isEmpty &&
    (!synthesisPresent || states.all (fun s => decide (isTerminal s)))

/-- Each witness's hand-written `legal` matches the computable predicate applied
    to its own fields. -/
def caseLegalCorrect (c : BarrierCase) : Bool :=
  c.legal == barrierLegal c.groupTerminalStates c.synthesisPresent

def workflowCases : List BarrierCase :=
  [ { name := "all_terminal_then_synthesis"
    , groupTerminalStates := [.completed, .completed, .cancelled]
    , synthesisPresent := true
    , legal := true
    }
  , { name := "failed_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .failed, .completed]
    , synthesisPresent := true
    , legal := true
    }
  , { name := "pending_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .running, .completed]
    , synthesisPresent := true
    , legal := false
    }
  , { name := "empty_group"
    , groupTerminalStates := []
    , synthesisPresent := true
    , legal := false
    }
    -- conf-3 (a): pre-barrier branch — a non-terminal sibling is legal as long
    -- as synthesis has NOT been spawned. Fences the Rust predicate's
    -- `!synthesis_present` short-circuit (and the `synthesis_present := false`
    -- serializer path).
  , { name := "running_sibling_no_synthesis"
    , groupTerminalStates := [.completed, .running, .completed]
    , synthesisPresent := false
    , legal := true
    }
    -- conf-3 (b): all-terminal INCLUDING a `.timedOut` sibling + synthesis.
    -- Fences the camelCase `timedOut` terminal-vocabulary string on both sides.
  , { name := "timed_out_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .timedOut, .cancelled]
    , synthesisPresent := true
    , legal := true
    }
  ]

/-- **Conformance lemma (conf-2).** Every entry of `workflowCases` carries a
    `legal` value entailed by the computable barrier predicate. Decided by
    `native_decide`, so a wrong `legal` annotation fails to build. -/
theorem workflowCasesLegalCorrect :
    workflowCases.all caseLegalCorrect = true := by
  native_decide

end Conformance
end Workflow
