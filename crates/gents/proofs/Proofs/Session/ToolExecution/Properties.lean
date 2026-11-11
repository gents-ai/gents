import Proofs.ToolExecution.Transition

namespace ToolExecution
namespace ToolCallContext

theorem terminal_irreversible
    {pre post : ToolCallContext}
    (h_terminal : isTerminal pre.state)
    (h_step : Transition pre post) :
    pre.state = post.state ∧ pre.failureClass = post.failureClass := by
  cases h_step <;> simp_all [isTerminal]

theorem cancellable_iff_non_terminal (c : ToolCallContext) :
    c.cancellable ↔ ¬ isTerminal c.state := by
  unfold cancellable
  cases c.state <;> simp [isTerminal]

theorem timedOut_requires_deadline_exceeded
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_pre  : pre.state ≠ .timedOut)
    (h_post : post.state = .timedOut) :
    pre.deadlineExceeded := by
  cases h_step <;> simp_all [isTerminal]

theorem completed_implies_committed
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_pre  : pre.state ≠ .completed)
    (h_post : post.state = .completed) :
    post.persistence = .committed := by
  cases h_step <;> simp_all [isTerminal]

theorem live_call_reaches_terminal
    (c : ToolCallContext)
    (h_live : ¬ isTerminal c.state) :
    ∃ post, Trace c post ∧ isTerminal post.state := by
  match h_state : c.state with
  | .pending =>
      let post : ToolCallContext := { c with state := .cancelled }
      have h_trans : Transition c post :=
        Transition.cancelBeforeDispatch .userCancelled (h_state := h_state) (h_post := rfl)
      exact ⟨post, Trace.step h_trans Trace.refl, Or.inr (Or.inr (Or.inr rfl))⟩
  | .running =>
      by_cases h_deadline : c.deadlineExceeded
      case pos =>
        let post : ToolCallContext := { c with state := .timedOut }
        have h_step : Transition c post :=
          Transition.timeout (h_state := h_state) (h_deadline := h_deadline) (h_post := rfl)
        exact ⟨post, Trace.step h_step Trace.refl, Or.inr (Or.inr (Or.inl rfl))⟩
      case neg =>
        let mid : ToolCallContext := { c with currentTime := c.deadline + 1 }
        have h_le : c.currentTime ≤ c.deadline + 1 := by
          have h_not_gt : ¬ c.currentTime > c.deadline := by
            unfold deadlineExceeded at h_deadline; exact h_deadline
          exact Nat.le_succ_of_le (Nat.le_of_not_lt h_not_gt)
        have h_step1 : Transition c mid :=
          Transition.timeAdvance (t := c.deadline + 1) (h_le := h_le) (h_post := rfl)
        let post : ToolCallContext := { mid with state := .timedOut }
        have h_mid_running : mid.state = .running := h_state
        have h_mid_deadline : mid.deadlineExceeded := by
          show mid.currentTime > mid.deadline
          simp only [mid]
          exact Nat.lt_succ_self c.deadline
        have h_step2 : Transition mid post :=
          Transition.timeout (h_state := h_mid_running) (h_deadline := h_mid_deadline) (h_post := rfl)
        exact ⟨post, Trace.step h_step1 (Trace.step h_step2 Trace.refl),
               Or.inr (Or.inr (Or.inl rfl))⟩
  | .completed => exact absurd (Or.inl h_state) h_live
  | .failed    => exact absurd (Or.inr (Or.inl h_state)) h_live
  | .timedOut  => exact absurd (Or.inr (Or.inr (Or.inl h_state))) h_live
  | .cancelled => exact absurd (Or.inr (Or.inr (Or.inr h_state))) h_live

theorem transition_preserves_requestId
    {pre post : ToolCallContext}
    (h_step : Transition pre post) :
    post.requestId = pre.requestId := by
  cases h_step <;> simp_all [isTerminal]

theorem transition_preserves_callId
    {pre post : ToolCallContext}
    (h_step : Transition pre post) :
    post.callId = pre.callId := by
  cases h_step <;> simp_all [isTerminal]

theorem dispatch_sets_startedAt
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_pre  : pre.state = .pending)
    (h_post : post.state = .running) :
    post.startedAt = some pre.currentTime := by
  cases h_step <;> simp_all [isTerminal]

theorem startedAt_preserved_outside_dispatch
    {pre post : ToolCallContext}
    (h_step  : Transition pre post)
    (h_not   : ¬ (pre.state = .pending ∧ post.state = .running)) :
    post.startedAt = pre.startedAt := by
  cases h_step <;> simp_all [isTerminal]

end ToolCallContext
end ToolExecution
