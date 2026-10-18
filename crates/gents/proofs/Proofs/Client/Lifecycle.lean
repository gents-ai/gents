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

theorem deriveAttempt_request_mapping
    {req : RequestSnapshot}
    (h_not_super : req.isSuperseded = false) :
    deriveAttempt ⟨req⟩ = match req.lifecycleState with
      | .workspaceBindingPending | .pending => .waitingForClaim
      | .claimed | .processing => .running
      | .completed => .completed
      | .failed | .dead => .failed
      | .superseded => .superseded
      | .interrupted => .interrupted := by
  cases req with
  | mk lifecycleState isSuperseded =>
    cases lifecycleState <;> simp_all [deriveAttempt]

theorem lifecycle_transition_monotonic
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post)
    (isSuperseded : Bool) :
    (deriveAttempt ⟨⟨post.state, isSuperseded⟩⟩).rank ≥
    (deriveAttempt ⟨⟨pre.state, isSuperseded⟩⟩).rank := by
  cases h_trans <;> subst_vars <;>
    cases isSuperseded <;> simp_all [deriveAttempt, ClientTurnState.rank]
  case dedup_lose.false h_state _ =>
    rcases h_state with h_pending | h_workspace <;>
      simp_all [deriveAttempt, ClientTurnState.rank]

theorem supersession_override_monotonic (state : RequestState) :
    (deriveAttempt ⟨⟨state, true⟩⟩).rank ≥
    (deriveAttempt ⟨⟨state, false⟩⟩).rank := by
  cases state <;> simp [deriveAttempt, ClientTurnState.rank]
