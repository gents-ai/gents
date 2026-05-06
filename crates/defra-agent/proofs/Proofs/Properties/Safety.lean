import Proofs.Composed

/-!
# Safety Properties S1-S6

Proofs that the ideal agent state machine satisfies safety invariants.
-/

open RequestState RequestContext ProcessState PersistenceState ComposedState

private theorem pending_not_terminal : ¬ isTerminal RequestState.pending := by
  intro h
  cases h with
  | inl h => cases h
  | inr h =>
    cases h with
    | inl h => cases h
    | inr h =>
      cases h with
      | inl h => cases h
      | inr h =>
        cases h with
        | inl h => cases h
        | inr h => cases h

private theorem claimed_not_terminal : ¬ isTerminal RequestState.claimed := by
  intro h
  cases h with
  | inl h => cases h
  | inr h =>
    cases h with
    | inl h => cases h
    | inr h =>
      cases h with
      | inl h => cases h
      | inr h =>
        cases h with
        | inl h => cases h
        | inr h => cases h

private theorem processing_not_terminal : ¬ isTerminal RequestState.processing := by
  intro h
  cases h with
  | inl h => cases h
  | inr h =>
    cases h with
    | inl h => cases h
    | inr h =>
      cases h with
      | inl h => cases h
      | inr h =>
        cases h with
        | inl h => cases h
        | inr h => cases h

private theorem inputRequired_not_terminal : ¬ isTerminal RequestState.inputRequired := by
  intro h
  cases h with
  | inl h => cases h
  | inr h =>
    cases h with
    | inl h => cases h
    | inr h =>
      cases h with
      | inl h => cases h
      | inr h =>
        cases h with
        | inl h => cases h
        | inr h => cases h

theorem terminal_irreversibility
    {pre post : RequestContext}
    (h_terminal : isTerminal pre.state)
    (h_trans : RequestContext.Transition pre post) :
    isTerminal post.state := by
  cases h_trans with
  | claim h_pre _ _ _ =>
    rw [h_pre] at h_terminal
    exact (pending_not_terminal h_terminal).elim
  | dedup_lose h_pre _ _ =>
    rw [h_pre] at h_terminal
    exact (pending_not_terminal h_terminal).elim
  | begin_inference h_pre _ _ =>
    rw [h_pre] at h_terminal
    exact (claimed_not_terminal h_terminal).elim
  | advance h_pre _ _ =>
    rw [h_pre] at h_terminal
    exact (processing_not_terminal h_terminal).elim
  | finish h_pre _ _ =>
    rw [h_pre] at h_terminal
    exact (processing_not_terminal h_terminal).elim
  | fail h_pre _ _ =>
    rw [h_pre] at h_terminal
    exact (processing_not_terminal h_terminal).elim
  | fail_before_stream h_pre _ _ =>
    rw [h_pre] at h_terminal
    exact (claimed_not_terminal h_terminal).elim
  | expire h_pre _ _ _ h_post =>
    rw [h_pre] at h_terminal
    exact (pending_not_terminal h_terminal).elim
  | interrupt_before_claim h_pre _ _ _ =>
    rw [h_pre] at h_terminal
    exact (pending_not_terminal h_terminal).elim
  | interrupt_claimed h_pre _ _ _ =>
    rw [h_pre] at h_terminal
    exact (claimed_not_terminal h_terminal).elim
  | interrupt_processing h_pre _ _ _ =>
    rw [h_pre] at h_terminal
    exact (processing_not_terminal h_terminal).elim

theorem progress_monotonic
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post) :
    post.progressSeq ≥ pre.progressSeq := by
  cases h_trans with
  | claim _ _ _ h_post =>
    simp [h_post]
  | dedup_lose _ _ h_post =>
    simp [h_post]
  | begin_inference _ _ h_post =>
    simp [h_post]
  | advance _ _ h_post =>
    simp [h_post]
  | finish _ _ h_post =>
    simp [h_post]
  | fail _ _ h_post =>
    simp [h_post]
  | fail_before_stream _ _ h_post =>
    simp [h_post]
  | expire _ _ _ _ h_post =>
    simp [h_post]
  | interrupt_before_claim _ _ _ h_post =>
    simp [h_post]
  | interrupt_claimed _ _ _ h_post =>
    simp [h_post]
  | interrupt_processing _ _ _ h_post =>
    simp [h_post]

theorem completed_not_deadline_expired
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post)
    (h_completed : post.state = .completed) :
    pre.state = .processing := by
  cases h_trans with
  | finish h_pre _ h_post =>
    exact h_pre
  | claim _ _ _ h_post =>
    simp [h_post] at h_completed
  | dedup_lose _ _ h_post =>
    simp [h_post] at h_completed
  | begin_inference _ _ h_post =>
    simp [h_post] at h_completed
  | advance h_pre _ h_post =>
    exact h_pre
  | fail _ _ h_post =>
    simp [h_post] at h_completed
  | fail_before_stream _ _ h_post =>
    simp [h_post] at h_completed
  | expire _ _ _ _ h_post =>
    simp [h_post] at h_completed
  | interrupt_before_claim _ _ _ h_post =>
    simp [h_post] at h_completed
  | interrupt_claimed _ _ _ h_post =>
    simp [h_post] at h_completed
  | interrupt_processing _ _ _ h_post =>
    simp [h_post] at h_completed

theorem recovery_blocks_claims
    {s s' : ComposedState}
    (h_recovering : s.process = .recovering)
    (h_pending : s.request.state = .pending)
    (h_trans : ComposedState.Transition s s') :
    s'.request.state = .pending ∨ isTerminal s'.request.state := by
  cases h_trans with
  | process_step _ h_req_eq _ _ =>
    left
    have : s'.request.state = s.request.state := congrArg RequestContext.state h_req_eq
    rw [this]
    exact h_pending
  | request_step _ _ _ _ h_guard =>
    have h_accepts := h_guard h_pending
    rw [h_recovering] at h_accepts
    exact absurd h_accepts (fun h => h)
  | persistence_step _ nextPersistence _ h_req_eq _ _ _ =>
    left
    have : s'.request.state = ({ s.request with persistence := nextPersistence }).state := by
      exact congrArg RequestContext.state h_req_eq
    simp at this
    rw [this]
    exact h_pending
  | call_step _ h_req_eq _ _ =>
    left
    have : s'.request.state = s.request.state := congrArg RequestContext.state h_req_eq
    rw [this]
    exact h_pending

theorem persistence_before_completion
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post)
    (h_completed : post.state = .completed) :
    post.persistence = .committed := by
  cases h_trans with
  | finish _ _ h_post =>
    simp [h_post]
  | claim _ _ _ h_post =>
    simp [h_post] at h_completed
  | dedup_lose _ _ h_post =>
    simp [h_post] at h_completed
  | begin_inference _ _ h_post =>
    simp [h_post] at h_completed
  | advance h_pre _ h_post =>
    simp [h_post, h_pre] at h_completed
  | fail _ _ h_post =>
    simp [h_post] at h_completed
  | fail_before_stream _ _ h_post =>
    simp [h_post] at h_completed
  | expire _ _ _ _ h_post =>
    simp [h_post] at h_completed
  | interrupt_before_claim _ _ _ h_post =>
    simp [h_post] at h_completed
  | interrupt_claimed _ _ _ h_post =>
    simp [h_post] at h_completed
  | interrupt_processing _ _ _ h_post =>
    simp [h_post] at h_completed

theorem deadline_structural_bound
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post)
    (h_completed : post.state = .completed) :
    ¬post.deadlineExceeded ∨ post.persistence = .committed := by
  exact Or.inr (persistence_before_completion h_trans h_completed)

/-- R-Int: request-local interrupt field monotonicity — once `interruptRequestedAt` is set on
    a request, no subsequent transition may clear or change it. The runtime
    treats this field as read-only; it is submitter-owned.

    The stronger unconditional form (no `h_set` hypothesis) holds because no
    transition ever rewrites `interruptRequestedAt`; `h_set` is retained in
    the signature to document the operational invariant consumers care about. -/
theorem interrupt_monotonicity
    {pre post : RequestContext}
    (_h_set : pre.interruptRequestedAt.isSome)
    (h_trans : RequestContext.Transition pre post) :
    post.interruptRequestedAt = pre.interruptRequestedAt := by
  cases h_trans <;> simp_all

/-- R-TTL: request-local TTL field monotonicity — `validUntil` is submitter-owned and never
    rewritten by any runtime transition, regardless of whether it was set. -/
theorem valid_until_monotonicity
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post) :
    post.validUntil = pre.validUntil := by
  cases h_trans <;> simp_all
