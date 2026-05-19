import Proofs.Background.Transition

/-!
# Subagent Executable Semantics

Finite bridge-event surface for conformance generation.

The current executable `step` rejects every event, and `step_refines_transition`
closes the vacuous refinement obligation.
-/

namespace Subagent
namespace BridgedState

/-- An event that selects which bridge Transition to apply. -/
inductive Event where
  | parent_step           (innerEventOpaque : Unit)
                            -- Opaque composed-state event payload.
  | child_step            (innerEventOpaque : Unit)
  | bridge_spawn          (newCallId : ToolExecution.ToolCallId)
                          (newChildRid : RequestId)
  | bridge_complete
  | bridge_failure
  | bridge_cancel_cascade
  deriving Repr

/-- Executable single-step. Returns `none` for every event in the current bridge
    executable surface. -/
def step (s : BridgedState) (e : Event) : Option BridgedState :=
  match e with
  | _ => none

/-- Soundness: every legal step refines a Transition. -/
theorem step_refines_transition
    (s s' : BridgedState) (e : Event)
    (h : step s e = some s') :
    Transition s s' := by
  -- The current `step` always returns `none`, so this hypothesis is vacuous.
  exact absurd h (by simp [step])

end BridgedState
end Subagent
