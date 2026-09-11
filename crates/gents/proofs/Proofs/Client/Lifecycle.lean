import Proofs.Client.Types

theorem deriveAttempt_total (view : AttemptView) :
    ∃ s : ClientTurnState, deriveAttempt view = s :=
  ⟨deriveAttempt view, rfl⟩

theorem deriveTurn_total
    {attempts : List AttemptView}
    (h : attempts ≠ []) :
    ∃ s : ClientTurnState, deriveTurn attempts = some s := by
  induction attempts with
  | nil => contradiction
  | cons head tail ih =>
    cases tail with
    | nil => exact ⟨deriveAttempt head, rfl⟩
    | cons h' t' =>
      simp [deriveTurn]
      exact ih (by simp)

theorem deriveAttempt_nonterminal_response_driven
    {req : RequestSnapshot}
    {resp : Option ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_state : req.lifecycleState = .workspaceBindingPending ∨ req.lifecycleState = .pending ∨
               req.lifecycleState = .claimed ∨
               req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired) :
    deriveAttempt ⟨req, resp⟩ = match resp with
      | some r => match r.status with
        | .complete => .completed
        | .error => .failed
        | .streaming => .streaming
      | none => .waitingForClaim := by
  cases req with
  | mk lifecycleState isSuperseded =>
    rcases h_state with h | h | h | h | h <;>
      cases h <;> cases h_not_super <;> rfl

/-- Monotonicity is over the request owner's transitions, without a client-local lifecycle. -/
theorem lifecycle_transition_monotonic
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post)
    (isSuperseded : Bool)
    (resp : Option ResponseSnapshot) :
    (deriveAttempt ⟨⟨post.state, isSuperseded⟩, resp⟩).rank ≥
    (deriveAttempt ⟨⟨pre.state, isSuperseded⟩, resp⟩).rank := by
  cases h_trans <;> subst_vars <;>
    cases isSuperseded <;> cases resp with
    | none => simp_all [deriveAttempt, ClientTurnState.rank]
    | some r => cases hstatus : r.status <;> simp_all [deriveAttempt, ClientTurnState.rank]

theorem response_advance_monotonic_none_to_some
    {req : RequestSnapshot}
    {resp : ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_nonterminal : req.lifecycleState = .workspaceBindingPending ∨
                     req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
                     req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired) :
    (deriveAttempt ⟨req, some resp⟩).rank ≥
    (deriveAttempt ⟨req, none⟩).rank := by
  rw [deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal,
      deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal]
  cases resp.status <;> simp [ClientTurnState.rank]

theorem response_advance_monotonic_streaming_to_terminal
    {req : RequestSnapshot}
    {resp_new : ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_nonterminal : req.lifecycleState = .workspaceBindingPending ∨
                     req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
                     req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired)
    (h_terminal : resp_new.status = .complete ∨ resp_new.status = .error) :
    (deriveAttempt ⟨req, some resp_new⟩).rank ≥
    (deriveAttempt ⟨req, some ⟨.streaming, false⟩⟩).rank := by
  rw [deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal,
      deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal]
  rcases h_terminal with h | h <;> simp [h, ClientTurnState.rank]
