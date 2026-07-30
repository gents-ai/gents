import Proofs.StreamingResponse.Properties.Lifecycle

namespace StreamingResponse

theorem normal_finalize_clears_liveTail
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_pre_streaming : pre.status = .streaming)
    (h_finalize : post.status = .completed ∨
                  (post.status = .error ∧
                   post.errorReason ≠ some .daemonRestartRecovery)) :
    post.liveTail = .empty := by
  cases h with
  | begin h_streaming h_emp _ _ h_post =>
    rw [h_post]; exact h_emp
  | writeTokens _ _ h_post =>
    rcases h_finalize with h_comp | ⟨h_err, _⟩
    · rw [h_post] at h_comp; simp at h_comp
      rw [h_pre_streaming] at h_comp; cases h_comp
    · rw [h_post] at h_err; simp at h_err
      rw [h_pre_streaming] at h_err; cases h_err
  | writeReasoning _ h_post =>
    rcases h_finalize with h_comp | ⟨h_err, _⟩
    · rw [h_post] at h_comp; simp at h_comp
      rw [h_pre_streaming] at h_comp; cases h_comp
    · rw [h_post] at h_err; simp at h_err
      rw [h_pre_streaming] at h_err; cases h_err
  | flushPending _ h_post =>
    rcases h_finalize with h_comp | ⟨h_err, _⟩
    · rw [h_post] at h_comp; rw [h_pre_streaming] at h_comp; cases h_comp
    · rw [h_post] at h_err; rw [h_pre_streaming] at h_err; cases h_err
  | resetTail _ h_post =>
    rw [h_post]
  | setInterruptedAt _ _ h_post =>
    rw [h_post] at h_finalize; simp at h_finalize
    rcases h_finalize with h_comp | ⟨h_err, _⟩
    · rw [h_pre_streaming] at h_comp; cases h_comp
    · rw [h_pre_streaming] at h_err; cases h_err
  | finalizeComplete _ h_post =>
    rw [h_post]
  | finalizeError _ _ _ h_post =>
    rw [h_post]
  | recoverInterrupted _ h_post =>
    rcases h_finalize with h_comp | ⟨_, h_err_neq⟩
    · rw [h_post] at h_comp; simp at h_comp
    · rw [h_post] at h_err_neq; simp at h_err_neq
  | observeIdempotentFinalize h_pre_term _ =>
    cases h_pre_term with
    | inl h => rw [h] at h_pre_streaming; cases h_pre_streaming
    | inr h => rw [h] at h_pre_streaming; cases h_pre_streaming

theorem recovery_path_preserves_liveTail
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_reason : post.errorReason = some .daemonRestartRecovery)
    (h_pre_no_recovery : pre.errorReason ≠ some .daemonRestartRecovery) :
    post.liveTail = pre.liveTail := by
  cases h with
  | begin _ _ _ _ h_post =>
    rw [h_post] at h_reason
    exact absurd h_reason h_pre_no_recovery
  | writeTokens _ _ h_post =>
    rw [h_post] at h_reason; simp at h_reason
    exact absurd h_reason h_pre_no_recovery
  | writeReasoning _ h_post =>
    rw [h_post] at h_reason; simp at h_reason
    exact absurd h_reason h_pre_no_recovery
  | flushPending _ h_post =>
    rw [h_post] at h_reason
    exact absurd h_reason h_pre_no_recovery
  | resetTail _ h_post =>
    rw [h_post] at h_reason; simp at h_reason
    exact absurd h_reason h_pre_no_recovery
  | setInterruptedAt _ _ h_post =>
    rw [h_post] at h_reason; simp at h_reason
    exact absurd h_reason h_pre_no_recovery
  | finalizeComplete _ h_post =>
    rw [h_post] at h_reason; simp at h_reason
    exact absurd h_reason h_pre_no_recovery
  | finalizeError _ h_reasons _ h_post =>
    rw [h_post] at h_reason; simp at h_reason
    rcases h_reasons with h | h | h | h <;> rw [h] at h_reason <;>
      simp at h_reason
  | recoverInterrupted _ h_post => rw [h_post]
  | observeIdempotentFinalize _ h_post =>
    rw [h_post] at h_reason
    exact absurd h_reason h_pre_no_recovery

theorem finalize_persists_durable_reasoning_and_clears_tail
    {pre post : ResponseContext} {seq : Transcript.Sequence}
    (_h : Transition pre post)
    (h_finalize : post = { pre with
        status := .completed
      , liveTail := .empty
      , durableReasoning := pre.tailReasoning
      , materializedMessageSequence := some seq }) :
    post.liveTail = .empty ∧ post.durableReasoning = pre.tailReasoning := by
  rw [h_finalize]
  exact ⟨rfl, rfl⟩

theorem finalizeComplete_copies_reasoning_then_clears
    {pre post : ResponseContext}
    (h_streaming : pre.status = .streaming)
    (h : Transition pre post)
    (h_completed : post.status = .completed) :
    post.liveTail = .empty ∧ post.durableReasoning = pre.tailReasoning := by
  cases h with
  | begin _ _ _ _ h_post =>
    rw [h_post] at h_completed; rw [h_streaming] at h_completed; cases h_completed
  | writeTokens _ _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
    rw [h_streaming] at h_completed; cases h_completed
  | writeReasoning _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
    rw [h_streaming] at h_completed; cases h_completed
  | flushPending _ h_post =>
    rw [h_post] at h_completed; rw [h_streaming] at h_completed; cases h_completed
  | resetTail _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
    rw [h_streaming] at h_completed; cases h_completed
  | setInterruptedAt _ _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
    rw [h_streaming] at h_completed; cases h_completed
  | finalizeComplete _ h_post =>
    rw [h_post]; exact ⟨rfl, rfl⟩
  | finalizeError _ _ _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
  | recoverInterrupted _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
  | observeIdempotentFinalize h_pre_term h_post =>
    cases h_pre_term with
    | inl h => rw [h] at h_streaming; cases h_streaming
    | inr h => rw [h] at h_streaming; cases h_streaming

theorem completed_liveTail_is_empty_one_step
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_pre_streaming : pre.status = .streaming)
    (h_completed : post.status = .completed) :
    post.liveTail = .empty ∧ post.materializedMessageSequence.isSome := by
  refine ⟨?_, ?_⟩
  · exact normal_finalize_clears_liveTail h h_pre_streaming (Or.inl h_completed)
  · exact completed_carries_materialized_handle h h_completed
      (Or.inl h_pre_streaming)

theorem completed_state_has_empty_liveTail
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_completed : post.status = .completed)
    (h_pre_wellformed :
       pre.status = .streaming ∨
       (pre.status = .completed ∧
        pre.liveTail = .empty ∧
        pre.materializedMessageSequence.isSome)) :
    post.liveTail = .empty ∧
    post.materializedMessageSequence.isSome := by
  refine ⟨?_, ?_⟩
  · cases h with
    | begin h_streaming _ _ _ h_post =>
      rw [h_post] at h_completed
      rw [h_streaming] at h_completed
      cases h_completed
    | writeTokens h_streaming _ h_post =>
      rw [h_post] at h_completed; simp at h_completed
      rw [h_streaming] at h_completed; cases h_completed
    | writeReasoning h_streaming h_post =>
      rw [h_post] at h_completed; simp at h_completed
      rw [h_streaming] at h_completed; cases h_completed
    | flushPending h_streaming h_post =>
      rw [h_post] at h_completed
      rw [h_streaming] at h_completed
      cases h_completed
    | resetTail h_streaming h_post =>
      rw [h_post] at h_completed; simp at h_completed
      rw [h_streaming] at h_completed; cases h_completed
    | setInterruptedAt _ _ h_post =>
      rw [h_post]
      rw [h_post] at h_completed
      simp at h_completed
      cases h_pre_wellformed with
      | inl h_pre_streaming =>
        rw [h_pre_streaming] at h_completed
        cases h_completed
      | inr h_pre_completed => exact h_pre_completed.2.1
    | finalizeComplete _ h_post => rw [h_post]
    | finalizeError _ _ _ h_post =>
      rw [h_post] at h_completed; simp at h_completed
    | recoverInterrupted _ h_post =>
      rw [h_post] at h_completed; simp at h_completed
    | observeIdempotentFinalize _ h_post =>
      rw [h_post]
      cases h_pre_wellformed with
      | inl h_pre_streaming =>
        rw [h_post] at h_completed
        rw [h_pre_streaming] at h_completed
        cases h_completed
      | inr h_pre_completed => exact h_pre_completed.2.1
  · exact completed_carries_materialized_handle h h_completed
      (h_pre_wellformed.imp id (fun h => ⟨h.1, h.2.2⟩))

private theorem trace_from_terminal_is_noop
    {pre post : ResponseContext}
    (h_trace : Trace pre post)
    (h_pre_term : isTerminal pre.status) :
    post = pre := by
  induction h_trace with
  | refl => rfl
  | @step s₁ s₂ s₃ h_trans _ ih =>
    have h_noop : s₂ = s₁ := idempotent_finalize_is_noop h_trans h_pre_term
    have h_s₂_term : isTerminal s₂.status := h_noop ▸ h_pre_term
    exact (ih h_s₂_term).trans h_noop

theorem recovery_state_liveTail_stable
    {pre post : ResponseContext}
    (h_trace : Trace pre post)
    (h_pre_status : pre.status = .error)
    (_h_pre_reason : pre.errorReason = some .daemonRestartRecovery)
    (_h_post_reason : post.errorReason = some .daemonRestartRecovery) :
    post.liveTail = pre.liveTail := by
  have h_pre_term : isTerminal pre.status := by
    rw [h_pre_status]; exact Or.inr rfl
  have h_eq : post = pre := trace_from_terminal_is_noop h_trace h_pre_term
  rw [h_eq]

end StreamingResponse
