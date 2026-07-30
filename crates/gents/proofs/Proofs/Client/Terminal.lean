import Proofs.Client.Lifecycle

def effectivelyTerminal (view : AttemptView) : Prop :=
  view.request.isSuperseded = true ∨
  view.request.lifecycleState = .completed ∨
  view.request.lifecycleState = .failed ∨
  view.request.lifecycleState = .superseded ∨
  view.request.lifecycleState = .dead ∨
  view.request.lifecycleState = .interrupted ∨
  (∃ r, view.response = some r ∧ (r.status = .complete ∨ r.status = .error))

instance (view : AttemptView) : Decidable (effectivelyTerminal view) := by
  unfold effectivelyTerminal
  infer_instance

theorem terminal_coherence (view : AttemptView) :
    (deriveAttempt view).isTerminal = true ↔ effectivelyTerminal view := by
  obtain ⟨req, resp⟩ := view
  constructor
  ·
    intro h_client_term
    unfold effectivelyTerminal
    cases h_super : req.isSuperseded
    ·
      by_cases h_is_terminal_lc :
          req.lifecycleState = .completed ∨ req.lifecycleState = .failed ∨
          req.lifecycleState = .superseded ∨ req.lifecycleState = .dead ∨
          req.lifecycleState = .interrupted
      ·
        rcases h_is_terminal_lc with h | h | h | h | h
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr (Or.inl h))
        · exact Or.inr (Or.inr (Or.inr (Or.inl h)))
        · exact Or.inr (Or.inr (Or.inr (Or.inr (Or.inl h))))
        · exact Or.inr (Or.inr (Or.inr (Or.inr (Or.inr (Or.inl h)))))
      ·
        have h_nonterm : req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
                         req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired := by
          cases h_lc : req.lifecycleState
          · exact Or.inl rfl
          · exact Or.inr (Or.inl rfl)
          · exact Or.inr (Or.inr (Or.inl rfl))
          · exact Or.inr (Or.inr (Or.inr rfl))
          · exact absurd (Or.inl h_lc) h_is_terminal_lc
          · exact absurd (Or.inr (Or.inl h_lc)) h_is_terminal_lc
          · exact absurd (Or.inr (Or.inr (Or.inl h_lc))) h_is_terminal_lc
          · exact absurd (Or.inr (Or.inr (Or.inr (Or.inl h_lc)))) h_is_terminal_lc
          · exact absurd (Or.inr (Or.inr (Or.inr (Or.inr h_lc)))) h_is_terminal_lc
        rw [deriveAttempt_nonterminal_response_driven h_super h_nonterm] at h_client_term
        right; right; right; right; right; right
        cases resp with
        | none => simp [ClientTurnState.isTerminal] at h_client_term
        | some r =>
          refine ⟨r, rfl, ?_⟩
          cases h_status : r.status
          · simp [h_status, ClientTurnState.isTerminal] at h_client_term
          · exact Or.inl rfl
          · exact Or.inr rfl
    ·
      exact Or.inl rfl
  ·
    intro h_eff
    cases h_super : req.isSuperseded
    ·
      rcases h_eff with h_super' | h_lc_comp | h_lc_fail | h_lc_super | h_lc_dead | h_lc_int | ⟨r, h_resp, h_status⟩
      ·
        simp only at h_super'
        rw [h_super] at h_super'
        exact absurd h_super' (by simp)
      · simp only at h_lc_comp
        simp [deriveAttempt, h_super, h_lc_comp, ClientTurnState.isTerminal]
      · simp only at h_lc_fail
        simp [deriveAttempt, h_super, h_lc_fail, ClientTurnState.isTerminal]
      · simp only at h_lc_super
        simp [deriveAttempt, h_super, h_lc_super, ClientTurnState.isTerminal]
      · simp only at h_lc_dead
        simp [deriveAttempt, h_super, h_lc_dead, ClientTurnState.isTerminal]
      · simp only at h_lc_int
        simp [deriveAttempt, h_super, h_lc_int, ClientTurnState.isTerminal]
      ·
        simp only at h_resp
        cases h_lc : req.lifecycleState
        case pending =>
          rw [deriveAttempt_nonterminal_response_driven h_super (Or.inl h_lc)]
          rw [h_resp]
          rcases h_status with h | h <;> simp [h, ClientTurnState.isTerminal]
        case claimed =>
          rw [deriveAttempt_nonterminal_response_driven h_super (Or.inr (Or.inl h_lc))]
          rw [h_resp]
          rcases h_status with h | h <;> simp [h, ClientTurnState.isTerminal]
        case processing =>
          rw [deriveAttempt_nonterminal_response_driven h_super (Or.inr (Or.inr (Or.inl h_lc)))]
          rw [h_resp]
          rcases h_status with h | h <;> simp [h, ClientTurnState.isTerminal]
        case inputRequired =>
          rw [deriveAttempt_nonterminal_response_driven h_super (Or.inr (Or.inr (Or.inr h_lc)))]
          rw [h_resp]
          rcases h_status with h | h <;> simp [h, ClientTurnState.isTerminal]
        case completed =>
          simp [deriveAttempt, h_super, h_lc, ClientTurnState.isTerminal]
        case failed =>
          simp [deriveAttempt, h_super, h_lc, ClientTurnState.isTerminal]
        case superseded =>
          simp [deriveAttempt, h_super, h_lc, ClientTurnState.isTerminal]
        case dead =>
          simp [deriveAttempt, h_super, h_lc, ClientTurnState.isTerminal]
        case interrupted =>
          simp [deriveAttempt, h_super, h_lc, ClientTurnState.isTerminal]
    ·
      simp [deriveAttempt, h_super, ClientTurnState.isTerminal]
