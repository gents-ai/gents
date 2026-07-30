import Proofs.Client.Terminal

theorem deriveTurn_deterministic
    (attempts : List AttemptView) :
    deriveTurn attempts = deriveTurn attempts := rfl

theorem turn_replacement_derives_new_tip
    (attempts : List AttemptView)
    (newTip : AttemptView) :
    deriveTurn (attempts ++ [newTip]) = some (deriveAttempt newTip) :=
  deriveTurn_append_singleton attempts newTip

theorem supersession_rank
    (view : AttemptView)
    (h_super : view.request.isSuperseded = true) :
    (deriveAttempt view).rank = 2 := by
  simp [deriveAttempt, h_super, ClientTurnState.rank]

theorem retry_restart_state
    (newTip : AttemptView)
    (h_pending : newTip.request.lifecycleState = .pending)
    (h_not_super : newTip.request.isSuperseded = false)
    (h_no_resp : newTip.response = none) :
    deriveAttempt newTip = .waitingForClaim := by
  simp [deriveAttempt, h_not_super, h_pending, h_no_resp]
