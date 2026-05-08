import Proofs.ToolExecution.Transition

/-!
# Tool Call Single-Machine Properties

T1..T5 — daemon-visible invariants over `ToolCallContext.Transition`.
Composition theorems C1, C1', C2, C3 live in `Proofs/Composed.lean`.
-/

namespace ToolExecution
namespace ToolCallContext

/-- T1: Terminal irreversibility. Once in completed/failed/timedOut/cancelled,
    no transition leaves the state or mutates the failureClass. -/
theorem terminal_irreversible
    {pre post : ToolCallContext}
    (h_terminal : isTerminal pre.state)
    (h_step : Transition pre post) :
    pre.state = post.state ∧ pre.failureClass = post.failureClass := by
  cases h_step with
  | dispatch h_state _              => simp_all [isTerminal]
  | spawnFailed _ h_state _         => simp_all [isTerminal]
  | complete h_state _ _            => simp_all [isTerminal]
  | fail _ h_state _                => simp_all [isTerminal]
  | timeout h_state _ _             => simp_all [isTerminal]
  | cancelBeforeDispatch h_state _  => simp_all [isTerminal]
  | cancelDuringRun h_state _       => simp_all [isTerminal]
  | timeAdvance _ _ h_post          => simp_all
  | persistenceStep _ _ _ h_post    => simp_all

end ToolCallContext
end ToolExecution
