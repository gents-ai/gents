import Proofs.RequestExecutionLease.Transition

namespace RequestExecutionLease

variable {Generation : Type} [DecidableEq Generation]

theorem progress_bounds_eligible_fact
    (facts : List (OutputFact Generation)) (fact : OutputFact Generation)
    (duration : Time) (hmem : fact ∈ facts)
    (heligible : fact.eligibility = .currentRequest) :
    fact.createdAt + duration ≤ progressDeadline fact.generation duration facts := by
  induction facts with
  | nil => simp at hmem
  | cons first rest ih =>
      simp only [List.mem_cons] at hmem
      rcases hmem with rfl | hmem
      · simpa [progressDeadline, heligible] using
          (Nat.le_max_left (fact.createdAt + duration)
            (progressDeadline fact.generation duration rest))
      · exact Nat.le_trans (ih hmem) (Nat.le_max_right _ _)

theorem recent_eligible_progress_prevents_expiry
    (world : World Generation) (fact : OutputFact Generation)
    (owner : Generation) (duration explicitDeadline : Time)
    (hlease : world.lease = .active owner duration explicitDeadline)
    (hmem : fact ∈ world.output) (hgen : fact.generation = owner)
    (heligible : fact.eligibility = .currentRequest)
    (hrecent : world.now < fact.createdAt + duration) :
    ¬ effectiveExpiry world ≤ world.now := by
  have hbound := progress_bounds_eligible_fact world.output fact duration hmem heligible
  rw [hgen] at hbound
  have hmax : progressDeadline owner duration world.output ≤ effectiveExpiry world := by
    simpa [effectiveExpiry, hlease] using
      (Nat.le_max_right explicitDeadline (progressDeadline owner duration world.output))
  exact Nat.not_le_of_gt (Nat.lt_of_lt_of_le hrecent (Nat.le_trans hbound hmax))

theorem claim_installs_generation_duration_and_deadline
    (pre post : World Generation) (generation : Generation)
    (duration deadline : Time)
    (h : step? pre (.claim .mutationWriteGate generation duration deadline) = some post) :
    post.request = .claimed ∧ post.lease = .active generation duration deadline ∧
      generation ∈ post.usedGenerations := by
  cases hlease : pre.lease with
  | vacant =>
      simp only [step?, hlease] at h
      split at h
      · cases h; simp
      · contradiction
  | active owner oldDuration oldDeadline => simp [step?, hlease] at h
  | recoverable owner oldDuration oldDeadline => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem claim_generation_was_fresh
    (pre post : World Generation) (generation : Generation)
    (duration deadline : Time)
    (h : step? pre (.claim .mutationWriteGate generation duration deadline) = some post) :
    fresh pre generation := by
  cases hlease : pre.lease with
  | vacant =>
      simp only [step?, hlease] at h
      split at h
      · rename_i hguard
        exact hguard.2.2.2.1
      · contradiction
  | active owner oldDuration oldDeadline => simp [step?, hlease] at h
  | recoverable owner oldDuration oldDeadline => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem append_output_stamps_once_without_explicit_renewal
    (pre post : World Generation) (generation : Generation)
    (id : CanonicalOutput.DocId)
    (h : step? pre (.appendOutput .mutationWriteGate generation id .currentRequest) =
      some post) :
    post = { pre with output :=
      ⟨id, generation, pre.now, .currentRequest⟩ :: pre.output } := by
  simp only [step?] at h
  split at h
  · contradiction
  · split at h
    · exact (Option.some.inj h).symm
    · contradiction

theorem replay_is_identity_and_never_renews
    (pre post : World Generation) (fact : OutputFact Generation)
    (h : step? pre (.replayOutput fact) = some post) :
    post = pre ∧ effectiveExpiry post = effectiveExpiry pre := by
  simp only [step?] at h
  split at h
  · cases h; exact ⟨rfl, rfl⟩
  · contradiction

theorem socket_traffic_does_not_renew
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.socketTraffic generation) = some post) : post = pre := by
  cases hlease : pre.lease <;> simp [step?, hlease] at h
  exact h.2.symm

theorem no_op_does_not_renew
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.noOp generation) = some post) : post = pre := by
  cases hlease : pre.lease <;> simp [step?, hlease] at h
  exact h.2.symm

theorem stale_generation_cannot_append
    (pre : World Generation) (owner stale : Generation)
    (duration deadline : Time) (id : CanonicalOutput.DocId)
    (hlease : pre.lease = .active owner duration deadline)
    (hstale : stale ≠ owner) :
    step? pre (.appendOutput .mutationWriteGate stale id .currentRequest) = none := by
  simp [step?, admitted, hlease, Ne.symm hstale]

theorem stale_generation_cannot_renew
    (pre : World Generation) (owner stale : Generation)
    (duration deadline : Time)
    (hlease : pre.lease = .active owner duration deadline)
    (hstale : stale ≠ owner) :
    step? pre (.renew .mutationWriteGate stale) = none := by
  simp [step?, admitted, hlease, Ne.symm hstale]

theorem stale_generation_cannot_finalize
    (pre : World Generation) (owner stale : Generation)
    (duration deadline : Time) (outcome : Outcome)
    (hlease : pre.lease = .active owner duration deadline)
    (hstale : stale ≠ owner) :
    step? pre (.finalize .mutationWriteGate stale outcome) = none := by
  simp [step?, admitted, hlease, Ne.symm hstale]

theorem expired_rejects_all_producer_actions
    (pre : World Generation) (owner : Generation)
    (duration deadline : Time) (id : CanonicalOutput.DocId)
    (decision : ProducerDecision) (outcome : Outcome)
    (hlease : pre.lease = .active owner duration deadline)
    (hexpired : effectiveExpiry pre ≤ pre.now) :
    step? pre (.begin .mutationWriteGate owner) = none ∧
      step? pre (.appendOutput .mutationWriteGate owner id .currentRequest) = none ∧
      step? pre (.renew .mutationWriteGate owner) = none ∧
      step? pre (.authorizeProducerDecision .mutationWriteGate owner decision) = none ∧
      step? pre (.finalize .mutationWriteGate owner outcome) = none := by
  have hnot : ¬ admitted pre .mutationWriteGate owner := by
    intro hadmitted
    unfold admitted at hadmitted
    rw [hlease] at hadmitted
    exact Nat.not_lt_of_ge hexpired hadmitted.2.2.2.2
  simp [step?, hlease, hnot]

theorem renewal_advances_prior_deadline
    (pre post : World Generation) (generation : Generation)
    (duration deadline : Time)
    (hlease : pre.lease = .active generation duration deadline)
    (h : step? pre (.renew .mutationWriteGate generation) = some post) :
    post.lease = .active generation duration (renewDeadline pre duration deadline) ∧
      deadline < renewDeadline pre duration deadline := by
  simp [step?, hlease] at h
  rcases h with ⟨_, rfl⟩
  exact ⟨rfl, Nat.lt_of_lt_of_le (Nat.lt_succ_self deadline) (Nat.le_max_right _ _)⟩

theorem expired_recovery_is_atomic_generation_swap
    (pre post : World Generation) (expected generation : Generation)
    (oldDuration oldDeadline duration deadline : Time)
    (hlease : pre.lease = .active expected oldDuration oldDeadline)
    (h : step? pre
      (.recoverExpired .mutationWriteGate expected generation duration deadline) = some post) :
    effectiveExpiry pre ≤ pre.now ∧ fresh pre generation ∧
      post.lease = .active generation duration deadline ∧
      generation ∈ post.usedGenerations := by
  simp [step?, hlease] at h
  rcases h with ⟨hguard, rfl⟩
  exact ⟨hguard.2.2.1, hguard.2.2.2.2.1, rfl, by simp [installFresh]⟩

theorem expired_recovery_enabled
    (pre : World Generation) (expected generation : Generation)
    (oldDuration oldDeadline duration deadline : Time)
    (hlease : pre.lease = .active expected oldDuration oldDeadline)
    (hintegrity : integrityHealthy pre) (hclock : clockCoherent pre expected)
    (hexpired : effectiveExpiry pre ≤ pre.now) (hduration : duration > 0)
    (hfresh : fresh pre generation) (hdeadline : pre.now < deadline) :
    ∃ post,
      step? pre (.recoverExpired .mutationWriteGate expected generation duration deadline) =
        some post := by
  refine ⟨installFresh pre generation duration deadline, ?_⟩
  simp [step?, hlease, hintegrity, hclock, hexpired, hduration, hfresh, hdeadline]

theorem observer_cannot_recover_expired
    (pre : World Generation) (expected generation : Generation)
    (duration deadline : Time) :
    step? pre (.recoverExpired .observingReplica expected generation duration deadline) = none := by
  cases hlease : pre.lease <;> simp [step?, hlease]

theorem terminalization_agrees_atomically
    (pre post : World Generation) (generation : Generation) (outcome : Outcome)
    (h : step? pre (.finalize .mutationWriteGate generation outcome) = some post) :
    post.lease = .terminal generation outcome ∧ post.request = outcome.requestState := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner duration deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      unfold admitted at hguard
      rw [hlease] at hguard
      have howner := hguard.1.2.2.1
      subst owner
      simp [terminalize, commitTerminalEffects]
  | recoverable owner duration deadline => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem terminal_state_rejects_second_finalize
    (pre post : World Generation) (generation other : Generation)
    (outcome otherOutcome : Outcome)
    (h : step? pre (.finalize .mutationWriteGate generation outcome) = some post) :
    step? post (.finalize .mutationWriteGate other otherOutcome) = none := by
  have hagreement := terminalization_agrees_atomically pre post generation outcome h
  simp [step?, hagreement.1]

theorem terminal_effects_at_most_once
    (pre post : World Generation) (generation : Generation) (outcome : Outcome)
    (h : step? pre (.finalize .mutationWriteGate generation outcome) = some post) :
    terminalEffectsBounded post := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner duration deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨_, rfl⟩
      simp [terminalEffectsBounded, terminalize, commitTerminalEffects]
      cases pre.continuationRequired <;> cases pre.tokenChargeRequired <;> simp_all
  | recoverable owner duration deadline => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem drop_relinquishes_matching_owner
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.drop .mutationWriteGate generation) = some post) :
    ∃ duration deadline, post.lease = .recoverable generation duration deadline := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner duration deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨rfl, rfl⟩
      exact ⟨duration, deadline, rfl⟩
  | recoverable owner duration deadline => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem dropped_recovery_installs_fresh_generation
    (pre post : World Generation) (expected generation : Generation)
    (duration deadline : Time)
    (h : step? pre
      (.recoverDropped .mutationWriteGate expected generation duration deadline) = some post) :
    fresh pre generation ∧ post.lease = .active generation duration deadline ∧
      generation ∈ post.usedGenerations := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner oldDuration oldDeadline => simp [step?, hlease] at h
  | recoverable owner oldDuration oldDeadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      exact ⟨hguard.2.2.2.2.1, rfl, by simp [installFresh]⟩
  | terminal owner outcome => simp [step?, hlease] at h

theorem expired_recovery_failure_is_fresh_atomic_and_bounded
    (pre post : World Generation) (expected generation : Generation)
    (h : step? pre
      (.recoverExpiredAndFail .mutationWriteGate expected generation) = some post) :
    fresh pre generation ∧ post.lease = .terminal generation .failed ∧
      post.request = .failed ∧ terminalEffectsBounded post := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner duration deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      refine ⟨hguard.2.2.2.2.1, rfl, rfl, ?_⟩
      simp [recoveryTerminal, terminalEffectsBounded, terminalize, commitTerminalEffects]
      cases pre.continuationRequired <;> cases pre.tokenChargeRequired <;> simp_all
  | recoverable owner duration deadline => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem policy_revocation_is_fresh_atomic
    (pre post : World Generation) (expected generation : Generation) (outcome : Outcome)
    (h : step? pre
      (.policyRevoke .mutationWriteGate expected generation outcome) = some post) :
    fresh pre generation ∧ post.lease = .terminal generation outcome ∧
      post.request = outcome.requestState := by
  cases hlease : pre.lease with
  | vacant => simp [step?, hlease] at h
  | active owner duration deadline =>
      simp [step?, hlease] at h
      rcases h with ⟨hguard, rfl⟩
      exact ⟨hguard.2.1, rfl, rfl⟩
  | recoverable owner duration deadline => simp [step?, hlease] at h
  | terminal owner oldOutcome => simp [step?, hlease] at h

theorem provider_eof_requires_explicit_final (sawExplicitFinal : Bool) :
    providerEofIsFailure sawExplicitFinal = false ↔ sawExplicitFinal = true := by
  cases sawExplicitFinal <;> simp [providerEofIsFailure]

theorem provider_eof_without_final_fails : providerEofIsFailure false = true := rfl

end RequestExecutionLease
