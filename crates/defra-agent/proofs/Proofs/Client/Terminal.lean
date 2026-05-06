import Proofs.Client.Lifecycle

/-!
# Client Terminal Coherence

Equivalence between terminal client views and effectively terminal request/response observations.
-/

/-! ## Theorem T3: Terminal Coherence

    The client view is terminal iff the server request is effectively
    terminal. "Effectively terminal" means:
    - The request is superseded (isSuperseded = true), OR
    - The lifecycle state is terminal (completed/failed/superseded/dead/interrupted), OR
    - The response status is terminal (complete/error)

    The third disjunct captures replication-lag tolerance: when the
    response has advanced past the request, the client should still
    correctly identify the turn as terminal.
-/

/-- Whether a request/response pair is effectively terminal from the
    server's perspective, accounting for replication lag. -/
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

/-- T3: The client view is terminal iff the attempt is effectively terminal.

    Structured as explicit case analysis over `req.isSuperseded` and
    `req.lifecycleState`, matching the per-arm discipline of
    `lifecycle_transition_monotonic` (T2). The forward direction uses
    `deriveAttempt_nonterminal_response_driven` once to collapse the four
    non-terminal lifecycle arms into a single shared response-case split.
    The backward direction enumerates all 12 lifecycle × response cases
    explicitly so that future changes to `deriveAttempt` produce a
    localized failure at the specific arm. -/
theorem terminal_coherence (view : AttemptView) :
    (deriveAttempt view).isTerminal = true ↔ effectivelyTerminal view := by
  obtain ⟨req, resp⟩ := view
  constructor
  · -- Forward: client terminal → effectively terminal
    intro h_client_term
    unfold effectivelyTerminal
    cases h_super : req.isSuperseded
    · -- isSuperseded = false: split on lifecycle. Terminal lifecycles are
      -- handled directly; non-terminal lifecycles use the response-driven
      -- helper so the per-response reasoning is shared rather than
      -- duplicated across the four non-terminal arms.
      by_cases h_is_terminal_lc :
          req.lifecycleState = .completed ∨ req.lifecycleState = .failed ∨
          req.lifecycleState = .superseded ∨ req.lifecycleState = .dead ∨
          req.lifecycleState = .interrupted
      · -- Terminal lifecycle: effectively terminal regardless of response.
        rcases h_is_terminal_lc with h | h | h | h | h
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr (Or.inl h))
        · exact Or.inr (Or.inr (Or.inr (Or.inl h)))
        · exact Or.inr (Or.inr (Or.inr (Or.inr (Or.inl h))))
        · exact Or.inr (Or.inr (Or.inr (Or.inr (Or.inr (Or.inl h)))))
      · -- Non-terminal lifecycle: derive the 4-way non-terminal disjunction
        -- from the negation of the terminal disjunction.
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
        -- Rewrite to the response-driven normal form, then case-split on
        -- response and status. This happens ONCE, replacing the four
        -- duplicated arms in the original proof.
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
    · -- isSuperseded = true
      exact Or.inl rfl
  · -- Backward: effectively terminal → client terminal.
    -- Explicit enumeration over the 12 lifecycle × response cases, matching
    -- the per-arm discipline of `lifecycle_transition_monotonic`. The four
    -- non-terminal arms use `deriveAttempt_nonterminal_response_driven` to
    -- normalize before consulting the response.
    intro h_eff
    cases h_super : req.isSuperseded
    · -- isSuperseded = false: dispatch on which disjunct of
      -- `effectivelyTerminal` holds.
      rcases h_eff with h_super' | h_lc_comp | h_lc_fail | h_lc_super | h_lc_dead | h_lc_int | ⟨r, h_resp, h_status⟩
      · -- isSuperseded = true contradicts h_super = false
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
      · -- Response terminal: enumerate the 9 lifecycle states explicitly.
        -- Terminal lifecycles: deriveAttempt is terminal regardless of response.
        -- Non-terminal lifecycles: use the helper to normalize, then consult
        -- the response (which h_status guarantees is terminal).
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
    · -- isSuperseded = true
      simp [deriveAttempt, h_super, ClientTurnState.isTerminal]
