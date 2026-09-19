import Proofs.Client.Terminal

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
    (h_not_super : newTip.request.isSuperseded = false) :
    deriveAttempt newTip = .waitingForClaim := by
  simp [deriveAttempt, h_not_super, h_pending]

theorem resolveRetryTip_accepts_unique
    (observationScope : String)
    (attempts : List KeyedAttemptView)
    (tip : KeyedAttemptView)
    (h : retryTips observationScope attempts = [tip]) :
    resolveRetryTip observationScope attempts = some tip := by
  simp [resolveRetryTip, h]

theorem resolveRetryTip_rejects_empty
    (observationScope : String)
    (attempts : List KeyedAttemptView)
    (h : retryTips observationScope attempts = []) :
    resolveRetryTip observationScope attempts = none := by
  simp [resolveRetryTip, h]

theorem resolveRetryTip_rejects_ambiguous
    (observationScope : String)
    (attempts : List KeyedAttemptView)
    (first second : KeyedAttemptView)
    (rest : List KeyedAttemptView)
    (h : retryTips observationScope attempts = first :: second :: rest) :
    resolveRetryTip observationScope attempts = none := by
  simp [resolveRetryTip, h]

theorem retryTips_preserve_observation_scope
    (observationScope : String)
    (attempts : List KeyedAttemptView)
    (tip : KeyedAttemptView)
    (h : tip ∈ retryTips observationScope attempts) :
    tip.observationScope = observationScope := by
  unfold retryTips attemptsInObservationScope at h
  simp only [List.mem_filter] at h
  exact of_decide_eq_true h.1.2

theorem resolveRetryTip_preserves_observation_scope
    (observationScope : String)
    (attempts : List KeyedAttemptView)
    (tip : KeyedAttemptView)
    (h : resolveRetryTip observationScope attempts = some tip) :
    tip.observationScope = observationScope := by
  unfold resolveRetryTip at h
  cases hTips : retryTips observationScope attempts with
  | nil => simp [hTips] at h
  | cons first rest =>
      cases rest with
      | nil =>
          simp [hTips] at h
          subst tip
          apply retryTips_preserve_observation_scope observationScope attempts first
          rw [hTips]
          simp
      | cons second tail => simp [hTips] at h

private def retryFixture
    (observationScope requestId : String)
    (retryParentRequest : Option String)
    (state : RequestState) : KeyedAttemptView :=
  { observationScope
  , requestId
  , retryParentRequest
  , attempt :=
      { request :=
          { lifecycleState := state
          , isSuperseded := false
          }
      }
  }

/-- The keyed resolver is insensitive to observation order for a simple retry
    chain because parenthood, rather than list position, selects the tip. -/
theorem unordered_retry_chain_resolves_tip_in_both_orders :
    let root := retryFixture "authorized-session" "root" none .failed
    let retry := retryFixture "authorized-session" "retry" (some "root") .pending
    resolveRetryTip "authorized-session" [root, retry] = some retry ∧
      resolveRetryTip "authorized-session" [retry, root] = some retry := by
  native_decide

theorem branching_with_multiple_retry_tips_is_rejected :
    let root := retryFixture "authorized-session" "root" none .failed
    let left := retryFixture "authorized-session" "left" (some "root") .pending
    let right := retryFixture "authorized-session" "right" (some "root") .pending
    resolveRetryTip "authorized-session" [root, left, right] = none := by
  native_decide

theorem closed_cycle_with_no_retry_tip_is_rejected :
    let left := retryFixture "authorized-session" "left" (some "right") .failed
    let right := retryFixture "authorized-session" "right" (some "left") .failed
    resolveRetryTip "authorized-session" [left, right] = none := by
  native_decide

theorem foreign_observation_scope_cannot_create_ambiguity :
    let localAttempt := retryFixture "authorized-session" "local" none .processing
    let foreign := retryFixture "other-session" "foreign" none .pending
    resolveRetryTip "authorized-session" [localAttempt, foreign] = some localAttempt := by
  native_decide
