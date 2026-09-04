import Proofs.RequestExecutionLease.Transition

namespace RequestExecutionLease

variable {Generation : Type} [DecidableEq Generation]

theorem claim_installs_generation_and_deadline
    (pre post : World Generation) (generation : Generation) (deadline : Time)
    (h : step? pre (.claim generation deadline) = some post) :
    post.request = .claimed ∧
      post.lease = .active generation deadline ∧
      generation ∈ post.usedGenerations := by
  cases hlease : pre.lease with
  | vacant =>
      simp [step?, hlease] at h
      rcases h with ⟨_, rfl⟩
      simp
  | active owner oldDeadline => simp [step?, hlease] at h
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem claim_generation_was_fresh
    (pre post : World Generation) (generation : Generation) (deadline : Time)
    (h : step? pre (.claim generation deadline) = some post) :
    fresh pre generation := by
  cases hlease : pre.lease with
  | vacant =>
      simp [step?, hlease] at h
      exact h.1.2.2.1
  | active owner oldDeadline => simp [step?, hlease] at h
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem claim_deadline_is_open
    (pre post : World Generation) (generation : Generation) (deadline : Time)
    (h : step? pre (.claim generation deadline) = some post) :
    pre.now < deadline := by
  cases hlease : pre.lease with
  | vacant =>
      simp [step?, hlease] at h
      exact h.1.2.2.2
  | active owner oldDeadline => simp [step?, hlease] at h
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem persisted_progress_renews_and_advances
    (pre post : World Generation) (generation : Generation)
    (kind : ProgressKind) (newDeadline : Time)
    (h : step? pre (.persistProgress generation kind newDeadline) = some post) :
    ∃ oldDeadline,
      pre.lease = .active generation oldDeadline ∧
      oldDeadline < newDeadline ∧
      post.lease = .active generation newDeadline ∧
      post.progressSeq = pre.progressSeq + 1 := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner oldDeadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      rcases hguard with ⟨rfl, _, hdeadline, _, _⟩
      exact ⟨oldDeadline, rfl, hdeadline, rfl, rfl⟩
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem socket_traffic_does_not_renew_or_advance
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.socketTraffic generation) = some post) :
    post = pre := by
  cases hlease : pre.lease <;> simp [step?, hlease] at h
  exact h.2.symm

theorem no_op_does_not_renew_or_advance
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.noOp generation) = some post) :
    post = pre := by
  cases hlease : pre.lease <;> simp [step?, hlease] at h
  exact h.2.symm

theorem stale_generation_cannot_renew
    (pre : World Generation) (owner stale : Generation) (deadline newDeadline : Time)
    (kind : ProgressKind)
    (hlease : pre.lease = .active owner deadline)
    (hstale : stale ≠ owner) :
    step? pre (.persistProgress stale kind newDeadline) = none := by
  simp [step?, hlease, Ne.symm hstale]

theorem stale_generation_cannot_finalize
    (pre : World Generation) (owner stale : Generation) (deadline : Time)
    (outcome : Outcome)
    (hlease : pre.lease = .active owner deadline)
    (hstale : stale ≠ owner) :
    step? pre (.finalize stale outcome) = none := by
  simp [step?, hlease, Ne.symm hstale]

theorem terminalization_agrees_atomically
    (pre post : World Generation) (generation : Generation) (outcome : Outcome)
    (h : step? pre (.finalize generation outcome) = some post) :
    post.lease = .terminal generation outcome ∧
      post.request = outcome.requestPhase ∧
      post.response = outcome.responsePhase := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      rcases hguard with ⟨rfl, _, _, _, _⟩
      simp [terminalize, commitTerminalEffects]
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem terminalization_records_matching_generation
    (pre post : World Generation) (generation : Generation) (outcome : Outcome)
    (h : step? pre (.finalize generation outcome) = some post) :
    ∃ deadline, pre.lease = .active generation deadline := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner deadline =>
      simp [step?, hlease] at h
      rcases h.1 with ⟨rfl, _, _, _, _⟩
      exact ⟨deadline, rfl⟩
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem terminal_state_rejects_second_finalize
    (pre post : World Generation) (generation other : Generation)
    (outcome otherOutcome : Outcome)
    (h : step? pre (.finalize generation outcome) = some post) :
    step? post (.finalize other otherOutcome) = none := by
  have hagreement := terminalization_agrees_atomically pre post generation outcome h
  simp [step?, hagreement.1]

theorem terminal_effects_at_most_once
    (pre post : World Generation) (generation : Generation) (outcome : Outcome)
    (h : step? pre (.finalize generation outcome) = some post) :
    terminalEffectsBounded post := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨_, rfl⟩
      simp [terminalEffectsBounded, terminalize, commitTerminalEffects]
      cases pre.continuationRequired <;> cases pre.tokenChargeRequired <;> simp
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem terminal_effects_belong_to_matching_winner
    (pre post : World Generation) (generation : Generation) (outcome : Outcome)
    (h : step? pre (.finalize generation outcome) = some post) :
    post.lease = .terminal generation outcome ∧
      post.continuationCount = (if pre.continuationRequired then 1 else 0) ∧
      post.tokenChargeCount = (if pre.tokenChargeRequired then 1 else 0) := by
  have hagreement := terminalization_agrees_atomically pre post generation outcome h
  refine ⟨hagreement.1, ?_⟩
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨_, rfl⟩
      simp [terminalize, commitTerminalEffects]
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem drop_relinquishes_matching_owner
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.drop generation) = some post) :
    post.lease = .recoverable generation := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨howner, rfl⟩
      subst owner
      rfl
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem expiry_relinquishes_matching_owner
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.expire generation) = some post) :
    post.lease = .recoverable generation := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      rcases hguard with ⟨rfl, _⟩
      rfl
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem recovery_installs_fresh_generation
    (pre post : World Generation) (expected generation : Generation)
    (deadline : Time)
    (h : step? pre (.recover expected generation deadline) = some post) :
    fresh pre generation ∧
      post.lease = .active generation deadline ∧
      generation ∈ post.usedGenerations := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner oldDeadline => simp [step?, hlease] at h
  | recoverable owner =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      rcases hguard with ⟨_, hfresh, _⟩
      exact ⟨hfresh, rfl, by simp⟩
  | terminal owner outcome => simp [step?, hlease] at h

theorem recovery_failure_is_fresh_atomic_and_bounded
    (pre post : World Generation) (expected generation : Generation)
    (h : step? pre (.recoverAndFail expected generation) = some post) :
    fresh pre generation ∧
      post.lease = .terminal generation .failed ∧
      post.request = .failed ∧
      post.response = .failed ∧
      terminalEffectsBounded post := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner deadline => simp [step?, hlease] at h
  | recoverable owner =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      rcases hguard with ⟨_, hfresh, _, _, _⟩
      refine ⟨hfresh, rfl, rfl, rfl, ?_⟩
      simp [terminalEffectsBounded, terminalize, commitTerminalEffects]
      cases pre.continuationRequired <;> cases pre.tokenChargeRequired <;> simp
  | terminal owner outcome => simp [step?, hlease] at h

theorem recovery_failure_rejects_second_winner
    (pre post : World Generation) (expected winner loser : Generation)
    (h : step? pre (.recoverAndFail expected winner) = some post) :
    step? post (.recoverAndFail expected loser) = none := by
  have hterminal := (recovery_failure_is_fresh_atomic_and_bounded
    pre post expected winner h).2.1
  simp [step?, hterminal]

/-- External policy revocation may revoke a live lease, but only the exact
observed tuple. It replaces the generation and commits the terminal pair. -/
theorem revocation_is_fresh_atomic_and_observed
    (pre post : World Generation) (expected generation : Generation)
    (deadline progress : Nat) (outcome : Outcome)
    (h : step? pre (.revoke expected deadline progress generation outcome) = some post) :
    pre.lease = .active expected deadline ∧ pre.progressSeq = progress ∧
      fresh pre generation ∧ post.lease = .terminal generation outcome ∧
      post.request = outcome.requestPhase ∧ post.response = outcome.responsePhase := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner oldDeadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      rcases hguard with ⟨rfl, rfl, hprogress, hfresh, _, _, _, _⟩
      exact ⟨rfl, hprogress, hfresh, rfl, rfl, rfl⟩
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem revocation_rejects_stale_progress
    (pre : World Generation) (owner generation : Generation)
    (deadline progress : Nat) (outcome : Outcome)
    (hlease : pre.lease = .active owner deadline)
    (hstale : pre.progressSeq ≠ progress) :
    step? pre (.revoke owner deadline progress generation outcome) = none := by
  simp [step?, hlease, hstale]

theorem revocation_rejects_second_terminal_winner
    (pre post : World Generation) (expected winner loser : Generation)
    (deadline progress : Nat) (outcome otherOutcome : Outcome)
    (h : step? pre (.revoke expected deadline progress winner outcome) = some post) :
    step? post (.finalize loser otherOutcome) = none ∧
      step? post (.revoke expected deadline progress loser otherOutcome) = none := by
  have hterminal := (revocation_is_fresh_atomic_and_observed
    pre post expected winner deadline progress outcome h).2.2.2.1
  simp [step?, hterminal]

theorem revocation_effects_are_bounded
    (pre post : World Generation) (expected generation : Generation)
    (deadline progress : Nat) (outcome : Outcome)
    (h : step? pre (.revoke expected deadline progress generation outcome) = some post) :
    terminalEffectsBounded post := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner oldDeadline =>
      simp [step?, hlease] at h
      rcases h with ⟨_, rfl⟩
      simp [terminalEffectsBounded, terminalize, commitTerminalEffects]
      cases pre.continuationRequired <;> cases pre.tokenChargeRequired <;> simp
  | recoverable owner => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem provider_eof_requires_explicit_final (sawExplicitFinal : Bool) :
    providerEofIsFailure sawExplicitFinal = false ↔ sawExplicitFinal = true := by
  cases sawExplicitFinal <;> simp [providerEofIsFailure]

theorem provider_eof_without_final_fails : providerEofIsFailure false = true := rfl

end RequestExecutionLease
