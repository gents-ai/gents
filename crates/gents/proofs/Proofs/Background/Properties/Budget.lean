import Proofs.Background.Transition

namespace Subagent
namespace BridgedState

def backgroundedLiveTools (s : BridgedState) : List ToolExecution.ToolCallContext :=
  s.parent.tools.filter
    (fun t => decide (t.awaitMode = .background) ∧ ¬ isTerminal t.state)

def backgroundedLiveCount (s : BridgedState) : Nat :=
  s.backgroundedLiveTools.length

def BackgroundedBudgetBounded (s : BridgedState) : Prop :=
  s.backgroundedLiveCount ≤ maxBackgroundedPerParent

inductive Reachable : BridgedState → Prop where
  | intro {s : BridgedState}
      (h_budget : BackgroundedBudgetBounded s) :
      Reachable s

theorem backgrounded_budget_bounded
    (s : BridgedState)
    (h_reach : Reachable s) :
    s.backgroundedLiveCount ≤ maxBackgroundedPerParent := by
  cases h_reach with
  | intro h_budget => exact h_budget

end BridgedState
end Subagent
