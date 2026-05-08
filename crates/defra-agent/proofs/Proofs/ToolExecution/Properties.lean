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


/-- T4: A call is cancellable iff its state is non-terminal. Operational
    meaning: any in-flight call accepts a cancel transition. -/
theorem cancellable_iff_non_terminal (c : ToolCallContext) :
    c.cancellable ↔ ¬ isTerminal c.state := by
  unfold cancellable
  cases c.state <;> simp [isTerminal]


/-- T2: TimedOut is reachable only when deadline is exceeded.
    The property whose absence in the runtime caused issue #149.
    The `h_pre` guard excludes `timeAdvance`/`persistenceStep`, which preserve
    state — they cannot be the transition that *enters* `.timedOut`. -/
theorem timedOut_requires_deadline_exceeded
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_pre  : pre.state ≠ .timedOut)
    (h_post : post.state = .timedOut) :
    pre.deadlineExceeded := by
  cases h_step with
  | dispatch _ h_post'              => simp_all
  | spawnFailed _ _ h_post'         => simp_all
  | complete _ _ h_post'            => simp_all
  | fail _ _ h_post'                => simp_all
  | timeout _ h_deadline _          => exact h_deadline
  | cancelBeforeDispatch _ h_post'  => simp_all
  | cancelDuringRun _ h_post'       => simp_all
  | timeAdvance _ _ h_post'         => simp_all
  | persistenceStep _ _ _ h_post'   => simp_all

/-- T3: Persistence before completion. Mirror of Request S6.
    The `h_pre` guard excludes `timeAdvance`/`persistenceStep`, which preserve
    state — they cannot be the transition that *enters* `.completed`. -/
theorem completed_implies_committed
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_pre  : pre.state ≠ .completed)
    (h_post : post.state = .completed) :
    post.persistence = .committed := by
  cases h_step with
  | dispatch _ h_post'              => simp_all
  | spawnFailed _ _ h_post'         => simp_all
  | complete _ h_persist h_post'    => simp_all
  | fail _ _ h_post'                => simp_all
  | timeout _ _ h_post'             => simp_all
  | cancelBeforeDispatch _ h_post'  => simp_all
  | cancelDuringRun _ h_post'       => simp_all
  | timeAdvance _ _ h_post'         => simp_all
  | persistenceStep _ _ _ h_post'   => simp_all

end ToolCallContext
end ToolExecution
