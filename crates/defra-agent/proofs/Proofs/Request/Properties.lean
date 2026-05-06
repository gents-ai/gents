import Proofs.Request.Executable

/-!
# Request Properties

Local invariants of request transitions and coherent request contexts.
-/

namespace RequestContext

theorem terminal_implies_released_local
    {r : RequestContext}
    (h_coherent : r.coherent)
    (h_term : isTerminal r.state) :
    r.admission = .released := by
  cases r with
  | mk state origin backend admission deadline claimTime currentTime retryCount maxRetries progressSeq messageSeq isLatest persistence interruptRequestedAt validUntil =>
    cases h_term with
    | inl h =>
      cases h
      cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
      rfl
    | inr h =>
      cases h with
      | inl h =>
        cases h
        cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
        rfl
      | inr h =>
        cases h with
        | inl h =>
          cases h
          cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
          rfl
        | inr h =>
          cases h with
          | inl h =>
            cases h
            cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
            rfl
          | inr h =>
            cases h
            cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent
            rfl

theorem backend_binding_preserved
    {pre post : RequestContext}
    (h_trans : Transition pre post) :
    post.backend = pre.backend := by
  cases h_trans with
  | claim _ _ h_post => rw [h_post]
  | dedup_lose _ _ h_post => rw [h_post]
  | begin_inference _ _ h_post => rw [h_post]
  | advance _ _ h_post => rw [h_post]
  | need_input _ _ h_post => rw [h_post]
  | input_received _ _ h_post => rw [h_post]
  | finish _ _ h_post => rw [h_post]
  | fail _ _ h_post => rw [h_post]
  | fail_before_stream _ _ h_post => rw [h_post]
  | input_timeout _ _ _ h_post => rw [h_post]
  | exhaust _ _ h_post => rw [h_post]
  | deadline_expire _ _ _ h_post => rw [h_post]
  | expire _ _ _ _ h_post => rw [h_post]
  | interrupt_before_claim _ _ _ h_post => rw [h_post]
  | interrupt_claimed _ _ _ h_post => rw [h_post]
  | interrupt_processing _ _ _ h_post => rw [h_post]
  | interrupt_input_required _ _ _ h_post => rw [h_post]

theorem origin_preserved
    {pre post : RequestContext}
    (h_trans : Transition pre post) :
    post.origin = pre.origin := by
  cases h_trans with
  | claim _ _ h_post => rw [h_post]
  | dedup_lose _ _ h_post => rw [h_post]
  | begin_inference _ _ h_post => rw [h_post]
  | advance _ _ h_post => rw [h_post]
  | need_input _ _ h_post => rw [h_post]
  | input_received _ _ h_post => rw [h_post]
  | finish _ _ h_post => rw [h_post]
  | fail _ _ h_post => rw [h_post]
  | fail_before_stream _ _ h_post => rw [h_post]
  | input_timeout _ _ _ h_post => rw [h_post]
  | exhaust _ _ h_post => rw [h_post]
  | deadline_expire _ _ _ h_post => rw [h_post]
  | expire _ _ _ _ h_post => rw [h_post]
  | interrupt_before_claim _ _ _ h_post => rw [h_post]
  | interrupt_claimed _ _ _ h_post => rw [h_post]
  | interrupt_processing _ _ _ h_post => rw [h_post]
  | interrupt_input_required _ _ _ h_post => rw [h_post]

theorem transition_produces_coherent
    {pre post : RequestContext}
    (h_trans : Transition pre post) :
    post.coherent := by
  cases h_trans with
  | claim _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | dedup_lose _ h_release h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_release]
  | begin_inference _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | advance h_state h_admission h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_state, h_admission]
  | need_input _ h_admission h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_admission]
  | input_received _ h_admission h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission, h_admission]
  | finish _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | fail _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | fail_before_stream _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | input_timeout _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | exhaust _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | deadline_expire _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | expire _ _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_before_claim _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_claimed _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_processing _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]
  | interrupt_input_required _ _ _ h_post =>
    rw [coherent, h_post]
    simp [coherentStateAdmission]

theorem claimed_coherent_cases
    {r : RequestContext}
    (h_state : r.state = .claimed)
    (h_coherent : r.coherent) :
    r.admission = .waiting ∨ r.admission = .acquired := by
  cases r with
  | mk state origin backend admission deadline claimTime currentTime retryCount maxRetries progressSeq messageSeq isLatest persistence interruptRequestedAt validUntil =>
    cases h_state
    cases admission <;> simp [coherent, coherentStateAdmission] at h_coherent ⊢


end RequestContext
