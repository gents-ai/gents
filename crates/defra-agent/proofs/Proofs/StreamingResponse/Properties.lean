import Proofs.StreamingResponse.Transition

/-!
# StreamingResponse Properties

State-machine basics, terminal-after-finalize (S6 bridge), stream-liveness
(L3 sibling), #64 live-tail clear with recovery asymmetry, uniqueness,
and idempotent finalize.
-/

namespace StreamingResponse

theorem terminal_irreversibility
    {pre post : ResponseContext}
    (h_term : isTerminal pre.status)
    (h_trans : Transition pre post) :
    isTerminal post.status := by
  cases h_trans with
  | begin h_streaming _ _ _ _ =>
    rw [h_streaming] at h_term
    cases h_term with
    | inl h => cases h
    | inr h => cases h
  | writeTokens h_streaming _ _ =>
    rw [h_streaming] at h_term
    cases h_term with
    | inl h => cases h
    | inr h => cases h
  | writeReasoning h_streaming _ =>
    rw [h_streaming] at h_term
    cases h_term with
    | inl h => cases h
    | inr h => cases h
  | flushPending h_streaming h_post =>
    rw [h_post]
    exact h_term
  | resetTail h_streaming _ =>
    rw [h_streaming] at h_term
    cases h_term with
    | inl h => cases h
    | inr h => cases h
  | setInterruptedAt _ _ h_post =>
    rw [h_post]
    simp
    exact h_term
  | finalizeComplete h_streaming _ =>
    rw [h_streaming] at h_term
    cases h_term with
    | inl h => cases h
    | inr h => cases h
  | finalizeError h_streaming _ _ _ =>
    rw [h_streaming] at h_term
    cases h_term with
    | inl h => cases h
    | inr h => cases h
  | recoverInterrupted h_streaming _ =>
    rw [h_streaming] at h_term
    cases h_term with
    | inl h => cases h
    | inr h => cases h
  | observeIdempotentFinalize _ h_post =>
    rw [h_post]
    exact h_term

theorem identity_preserved
    {pre post : ResponseContext}
    (h : Transition pre post) :
    pre.docId = post.docId ∧ pre.requestId = post.requestId := by
  cases h with
  | begin _ _ _ _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | writeTokens _ _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | writeReasoning _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | flushPending _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | resetTail _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | setInterruptedAt _ _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | finalizeComplete _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | finalizeError _ _ _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | recoverInterrupted _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
  | observeIdempotentFinalize _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩

theorem status_flow_bounded
    {pre post : ResponseContext}
    (h : Transition pre post) :
    (pre.status = .streaming → post.status = .streaming ∨ isTerminal post.status) ∧
    (isTerminal pre.status → post.status = pre.status) := by
  refine ⟨?_, ?_⟩
  · intro h_pre_streaming
    cases h with
    | begin _ _ _ _ h_post => left; rw [h_post]; exact h_pre_streaming
    | writeTokens h_streaming _ h_post => left; rw [h_post]; exact h_streaming
    | writeReasoning h_streaming h_post => left; rw [h_post]; exact h_streaming
    | flushPending h_streaming h_post => left; rw [h_post]; exact h_streaming
    | resetTail h_streaming h_post => left; rw [h_post]; exact h_streaming
    | setInterruptedAt _ _ h_post => left; rw [h_post]; exact h_pre_streaming
    | finalizeComplete _ h_post => right; rw [h_post]; exact Or.inl rfl
    | finalizeError _ _ _ h_post => right; rw [h_post]; exact Or.inr rfl
    | recoverInterrupted _ h_post => right; rw [h_post]; exact Or.inr rfl
    | observeIdempotentFinalize h_pre_term h_post =>
      rw [h_post]
      cases h_pre_term with
      | inl h_completed => rw [h_completed] at h_pre_streaming; cases h_pre_streaming
      | inr h_error => rw [h_error] at h_pre_streaming; cases h_pre_streaming
  · intro h_term
    cases h with
    | begin h_streaming _ _ _ _ =>
      rw [h_streaming] at h_term
      cases h_term with
      | inl h => cases h
      | inr h => cases h
    | writeTokens h_streaming _ _ =>
      rw [h_streaming] at h_term
      cases h_term with
      | inl h => cases h
      | inr h => cases h
    | writeReasoning h_streaming _ =>
      rw [h_streaming] at h_term
      cases h_term with
      | inl h => cases h
      | inr h => cases h
    | flushPending _ h_post => rw [h_post]
    | resetTail h_streaming _ =>
      rw [h_streaming] at h_term
      cases h_term with
      | inl h => cases h
      | inr h => cases h
    | setInterruptedAt _ _ h_post => rw [h_post]
    | finalizeComplete h_streaming _ =>
      rw [h_streaming] at h_term
      cases h_term with
      | inl h => cases h
      | inr h => cases h
    | finalizeError h_streaming _ _ _ =>
      rw [h_streaming] at h_term
      cases h_term with
      | inl h => cases h
      | inr h => cases h
    | recoverInterrupted h_streaming _ =>
      rw [h_streaming] at h_term
      cases h_term with
      | inl h => cases h
      | inr h => cases h
    | observeIdempotentFinalize _ h_post => rw [h_post]

theorem completed_carries_materialized_handle
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_completed : post.status = .completed)
    (h_pre : pre.status = .streaming ∨
             (pre.status = .completed ∧ pre.materializedMessageSequence.isSome)) :
    post.materializedMessageSequence.isSome := by
  cases h with
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
    rw [h_streaming] at h_completed; cases h_completed
  | resetTail h_streaming h_post =>
    rw [h_post] at h_completed; simp at h_completed
    rw [h_streaming] at h_completed; cases h_completed
  | setInterruptedAt _ _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
    rw [h_post]; simp
    cases h_pre with
    | inl h_pre_streaming =>
      rw [h_pre_streaming] at h_completed; cases h_completed
    | inr h_pre_completed => exact h_pre_completed.2
  | finalizeComplete _ h_post =>
    rw [h_post]; simp
  | finalizeError _ _ _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
  | recoverInterrupted _ h_post =>
    rw [h_post] at h_completed; simp at h_completed
  | observeIdempotentFinalize _ h_post =>
    rw [h_post]; rw [h_post] at h_completed
    cases h_pre with
    | inl h_pre_streaming =>
      rw [h_pre_streaming] at h_completed; cases h_completed
    | inr h_pre_completed => exact h_pre_completed.2

theorem response_completed_implies_request_committed
    {pre post : ResponseRequestBridge}
    (h : BridgeTransition pre post)
    (h_completed : post.response.status = .completed) :
    post.requestState = .completed ∧ post.requestPersistence = .committed := by
  cases h with
  | finalizeComplete _ _ _ h_req h_pers =>
    exact ⟨h_req, h_pers⟩
  | finalizeError _ _ _ h_eq _ _ _ =>
    rw [h_eq] at h_completed
    simp at h_completed
  | recoverPaired _ h_eq _ _ _ =>
    rw [h_eq] at h_completed
    simp at h_completed

theorem streamIdle_eventually_terminal
    (pre : ResponseContext)
    (h_streaming : pre.status = .streaming)
    (h_expired : pre.now > pre.streamIdleDeadline) :
    ∃ post, Transition pre post ∧ post.status = .error ∧
            post.errorReason = some .streamIdleTimeout := by
  refine ⟨{ pre with
    status := .error
  , liveTail := .empty
  , errorReason := some .streamIdleTimeout }, ?_, ?_, ?_⟩
  · exact Transition.finalizeError h_streaming
      (Or.inr (Or.inr (Or.inl rfl)))
      (fun _ => h_expired)
      rfl
  · rfl
  · rfl

theorem streaming_eventually_terminal
    (pre : ResponseContext)
    (h_streaming : pre.status = .streaming) :
    ∃ post, Transition pre post ∧ isTerminal post.status := by
  refine ⟨{ pre with
    status := .error
  , errorReason := some .daemonRestartRecovery }, ?_, ?_⟩
  · exact Transition.recoverInterrupted h_streaming rfl
  · exact Or.inr rfl

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

def BeginUniquePerRequestId (rows : List ResponseContext) : Prop :=
  ∀ r₁ r₂, r₁ ∈ rows → r₂ ∈ rows →
    r₁.requestId = r₂.requestId → r₁.docId = r₂.docId

theorem begin_preserves_unique_per_request_id
    (rows : List ResponseContext) (new : ResponseContext)
    (h_unique : BeginUniquePerRequestId rows)
    (h_no_existing : ∀ r, r ∈ rows → r.requestId ≠ new.requestId) :
    BeginUniquePerRequestId (new :: rows) := by
  intro r₁ r₂ h₁ h₂ h_req_eq
  simp at h₁ h₂
  rcases h₁ with h₁ | h₁
  · rcases h₂ with h₂ | h₂
    · rw [h₁, h₂]
    · exfalso
      have := h_no_existing r₂ h₂
      rw [h₁] at h_req_eq
      exact this h_req_eq.symm
  · rcases h₂ with h₂ | h₂
    · exfalso
      have := h_no_existing r₁ h₁
      rw [h₂] at h_req_eq
      exact this h_req_eq
    · exact h_unique r₁ r₂ h₁ h₂ h_req_eq

theorem idempotent_finalize_is_noop
    {pre post : ResponseContext}
    (h : Transition pre post)
    (h_pre_term : isTerminal pre.status) :
    post = pre := by
  cases h with
  | begin h_streaming _ _ _ _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | writeTokens h_streaming _ _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | writeReasoning h_streaming _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | flushPending _ h_post => exact h_post
  | resetTail h_streaming _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | setInterruptedAt h_streaming _ _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | finalizeComplete h_streaming _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | finalizeError h_streaming _ _ _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | recoverInterrupted h_streaming _ =>
    rw [h_streaming] at h_pre_term
    cases h_pre_term with
    | inl h => cases h
    | inr h => cases h
  | observeIdempotentFinalize _ h_post => exact h_post

end StreamingResponse
