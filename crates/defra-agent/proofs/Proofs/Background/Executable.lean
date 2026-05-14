import Proofs.Background.Transition

/-!
# Subagent Executable Semantics

A computable `step` function corresponding to each `BridgedState.Transition`
constructor, plus a `step_refines_transition` theorem that proves the function
implements the relation.

Used by Rust conformance generation to enumerate legal traces.

**Status:** scaffolding. The full `step` implementation requires
`ComposedState.Event` and `ComposedState.step` which may not exist in the
current codebase. The implementation here provides type scaffolding (the `Event`
enum and a placeholder `step`) with `sorry` for the refinement theorem; full
implementation is tracked as a follow-up alongside Rust runtime / conformance
JSON emission.
-/

namespace Subagent
namespace BridgedState

/-- An event that selects which bridge Transition to apply. -/
inductive Event where
  | parent_step           (innerEventOpaque : Unit)
                            -- ComposedState.Event placeholder; full enumeration deferred
  | child_step            (innerEventOpaque : Unit)
  | bridge_spawn          (newCallId : ToolExecution.ToolCallId)
                          (newChildRid : RequestId)
  | bridge_complete
  | bridge_failure
  | bridge_cancel_cascade
  deriving Repr

/-- Executable single-step. Returns `none` if the event isn't legal in the
    current state. Currently a stub — returns `none` for every event;
    full implementation deferred (see file docstring). -/
def step (s : BridgedState) (e : Event) : Option BridgedState :=
  match e with
  | _ => none

/-- Soundness: every legal step refines a Transition. -/
theorem step_refines_transition
    (s s' : BridgedState) (e : Event)
    (h : step s e = some s') :
    Transition s s' := by
  -- The current `step` always returns `none`, so this hypothesis is vacuous.
  -- When `step` is filled in, this proof discharges per-arm by applying the
  -- matching Transition constructor.
  exact absurd h (by simp [step])

end BridgedState
end Subagent
