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


/-- T5: Bounded reachability to terminal (liveness). Any non-terminal call
    has a 1- or 2-step trace to a terminal state, given a sufficient time
    advance. Daemon-side liveness underlying issue #149's fix. -/
theorem live_call_reaches_terminal
    (c : ToolCallContext)
    (h_live : ¬ isTerminal c.state) :
    ∃ post, Trace c post ∧ isTerminal post.state := by
  -- Two non-terminal states: .pending and .running.
  -- .pending  → cancelBeforeDispatch → .cancelled (terminal)
  -- .running  → timeAdvance(deadline+1); timeout → .timedOut (terminal)
  match h_state : c.state with
  | .pending =>
      let post : ToolCallContext := { c with state := .cancelled }
      have h_trans : Transition c post :=
        Transition.cancelBeforeDispatch (h_state := h_state) (h_post := rfl)
      exact ⟨post, Trace.step h_trans Trace.refl, Or.inr (Or.inr (Or.inr rfl))⟩
  | .running =>
      -- If deadline is already exceeded, timeout in 1 step; otherwise advance time first.
      by_cases h_deadline : c.deadlineExceeded
      case pos =>
        -- Already past deadline: timeout directly
        let post : ToolCallContext := { c with state := .timedOut }
        have h_step : Transition c post :=
          Transition.timeout (h_state := h_state) (h_deadline := h_deadline) (h_post := rfl)
        exact ⟨post, Trace.step h_step Trace.refl, Or.inr (Or.inr (Or.inl rfl))⟩
      case neg =>
        -- Not yet past deadline: advance time to deadline+1, then timeout
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

end ToolCallContext
end ToolExecution
