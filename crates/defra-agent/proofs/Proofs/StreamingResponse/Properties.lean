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
  | setInterruptedAt _ h_post =>
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
  | setInterruptedAt _ h_post => rw [h_post]; exact ⟨rfl, rfl⟩
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
    | setInterruptedAt _ h_post => left; rw [h_post]; exact h_pre_streaming
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
    | setInterruptedAt _ h_post => rw [h_post]
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

end StreamingResponse
