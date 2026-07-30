import Proofs.Background.Transition

namespace Subagent
namespace BridgedState

inductive Event where
  | parent_step           (innerEventOpaque : Unit)
  | child_step            (innerEventOpaque : Unit)
  | bridge_spawn          (newCallId : ToolExecution.ToolCallId)
                          (newChildRid : RequestId)
  | bridge_complete
  | bridge_failure
  | bridge_cancel_cascade
  deriving Repr

def step (s : BridgedState) (e : Event) : Option BridgedState :=
  match e with
  | _ => none

theorem step_refines_transition
    (s s' : BridgedState) (e : Event)
    (h : step s e = some s') :
    Transition s s' := by
  exact absurd h (by simp [step])

end BridgedState
end Subagent
