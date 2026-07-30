import Proofs.StreamingResponse.Transition

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
