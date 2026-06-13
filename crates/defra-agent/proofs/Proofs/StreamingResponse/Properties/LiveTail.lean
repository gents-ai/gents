import Proofs.StreamingResponse.Properties.Lifecycle

/-! Live-tail clearing and recovery-asymmetry properties for streaming responses. -/

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

/-- The recovery path preserves liveTail when the recovery errorReason is
freshly introduced by the transition. The hypothesis `h_pre_no_recovery`
encodes the runtime well-formedness invariant that a streaming response
cannot already carry the recovery error reason — only `recoverInterrupted`
introduces it. -/
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

/-- Issue #492 durable-reasoning persistence, co-held with the #64
live-tail clear. On a `finalizeComplete` materialize step, BOTH invariants
hold simultaneously:

* the live `liveTail` STILL clears to `.empty` (issue #64 contract), and
* the reasoning present in the live tail (`pre.tailReasoning`) is durably
  copied into `post.durableReasoning` — the formal model of writing the
  reasoning into the materialized `AgentMessage.reasoning` field.

This is the load-bearing proof for PR #492: the durable copy is a NEW,
separate persistence captured AT materialize time (`durableReasoning :=
pre.tailReasoning`), NOT a relaxation of the tail-clear. -/
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

/-- The reachable form: any `finalizeComplete` step both clears the live
tail to `.empty` and persists the durable reasoning copy. Stated directly
over the `Transition.finalizeComplete` constructor so the contract is tied
to the actual transition relation, not just a record shape. -/
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

/-- Audit-visible #64 corollary: any transition into a `.completed`
post-state leaves the live tail empty, given the pre-state is
well-formed (either fresh-streaming, or already-completed with the
post-finalize invariants preserved).

This composes `completed_liveTail_is_empty_one_step` with
`terminal_irreversibility`. Once a response reaches `.completed`, the
only legal subsequent transition is `observeIdempotentFinalize`
(which preserves the state), so the empty-liveTail / materialized-handle
property holds along any well-formed trace — making this corollary the
practical Trace-equivalent of the #64 sentinel without needing a
`TraceCoherent` predicate. -/
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
      -- post.liveTail = pre.liveTail; post.status = pre.status; we
      -- conclude empty from h_pre_wellformed (the inr branch, since
      -- post.status = .completed forces pre.status = .completed).
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

/-- Helper: any trace starting from a terminal state is a no-op trace
(every step is `observeIdempotentFinalize` or its parity, and produces
`post = pre`). Used to lift one-step terminal-irreversibility / idempotent
no-op into a Trace-level statement. -/
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

/-- Audit-visible recovery-asymmetry corollary: along any trace from a
recovery-state pre, `liveTail` is stable.

Mirrors `completed_state_has_empty_liveTail`'s role (the Trace-level
corollary for the #64 sentinel). Composes the single-step
`recovery_path_preserves_liveTail` with `terminal_irreversibility`:
a `pre.status = .error` is terminal, so by `idempotent_finalize_is_noop`,
every transition in the trace produces `post = pre`, and `liveTail`
is preserved along the entire trace.

We require `pre.status = .error` explicitly. The natural state of a
context with `errorReason = some .daemonRestartRecovery` IS `.error`
(set by `Transition.recoverInterrupted`), but proving this from
`errorReason` alone would require a global trace-well-formedness
invariant; adding the explicit terminal hypothesis is the cleaner
shape and matches how `completed_state_has_empty_liveTail` requires
its `pre.status = .completed` disjunct. -/
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
