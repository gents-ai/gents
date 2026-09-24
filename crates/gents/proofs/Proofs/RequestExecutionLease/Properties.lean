import Proofs.RequestExecutionLease.Transition

namespace RequestExecutionLease

variable {Generation : Type} [DecidableEq Generation]

theorem claim_installs_generation_duration_and_deadline
    (pre post : World Generation) (generation : Generation) (duration deadline : Time)
    (h : step? pre (.claim .mutationWriteGate generation duration deadline) = some post) :
    post.request = .claimed ∧ post.lease = .active generation duration deadline ∧
      generation ∈ post.usedGenerations := by
  cases hlease : pre.lease <;> simp [step?, hlease] at h
  rcases h with ⟨_, rfl⟩
  simp

theorem claim_generation_was_fresh
    (pre post : World Generation) (generation : Generation) (duration deadline : Time)
    (h : step? pre (.claim .mutationWriteGate generation duration deadline) = some post) :
    fresh pre generation := by
  cases hlease : pre.lease with
  | vacant =>
      simp only [step?, hlease] at h
      split at h
      · rename_i hguard
        exact hguard.2.2.2.1
      · contradiction
  | active owner duration deadline => simp [step?, hlease] at h
  | recoverable owner duration deadline => simp [step?, hlease] at h
  | terminal owner outcome => simp [step?, hlease] at h

theorem append_output_is_identity_and_never_renews
    (pre post : World Generation) (generation : Generation)
    (h : step? pre (.appendOutput .mutationWriteGate generation) = some post) :
    post = pre ∧ effectiveExpiry post = effectiveExpiry pre := by
  simp only [step?] at h
  split at h
  · cases h; exact ⟨rfl, rfl⟩
  · contradiction

theorem producer_decision_is_identity_and_never_renews
    (pre post : World Generation) (generation : Generation) (decision : ProducerDecision)
    (h : step? pre (.authorizeProducerDecision .mutationWriteGate generation decision) =
      some post) : post = pre ∧ effectiveExpiry post = effectiveExpiry pre := by
  cases hlease : pre.lease <;> simp [step?, hlease] at h
  exact ⟨h.2.symm, congrArg effectiveExpiry h.2.symm⟩

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
    (pre : World Generation) (owner stale : Generation) (duration deadline : Time)
    (hlease : pre.lease = .active owner duration deadline) (hstale : stale ≠ owner) :
    step? pre (.appendOutput .mutationWriteGate stale) = none := by
  simp [step?, admitted, hlease, Ne.symm hstale]

theorem stale_generation_cannot_renew
    (pre : World Generation) (owner stale : Generation)
    (duration deadline expectedDeadline : Time)
    (hlease : pre.lease = .active owner duration deadline) (hstale : stale ≠ owner) :
    step? pre (.renew .mutationWriteGate stale expectedDeadline) = none := by
  simp [step?, admitted, hlease, Ne.symm hstale]

theorem expired_rejects_all_producer_actions
    (pre : World Generation) (owner : Generation) (duration deadline : Time)
    (decision : ProducerDecision) (outcome : Outcome)
    (hlease : pre.lease = .active owner duration deadline) (hexpired : deadline ≤ pre.now) :
    step? pre (.begin .mutationWriteGate owner) = none ∧
      step? pre (.appendOutput .mutationWriteGate owner) = none ∧
      step? pre (.renew .mutationWriteGate owner deadline) = none ∧
      step? pre (.authorizeProducerDecision .mutationWriteGate owner decision) = none ∧
      step? pre (.finalize .mutationWriteGate owner outcome) = none := by
  have hnot : ¬ admitted pre .mutationWriteGate owner := by
    simp [admitted, hlease, Nat.not_lt_of_ge hexpired]
  simp [step?, hlease, hnot]

theorem renewal_success_is_due_exact_cas_and_advances
    (pre post : World Generation) (generation : Generation)
    (duration deadline expectedDeadline : Time)
    (hlease : pre.lease = .active generation duration deadline)
    (h : step? pre (.renew .mutationWriteGate generation expectedDeadline) = some post) :
    expectedDeadline = deadline ∧ renewalDue duration deadline ≤ pre.now ∧
      pre.now < deadline ∧ deadline < pre.now + duration ∧
      post.lease = .active generation duration (pre.now + duration) := by
  simp [step?, hlease, admitted, renewDeadline] at h
  rcases h with ⟨⟨hlive, hcas, hdue, hadvance, _⟩, rfl⟩
  exact ⟨hcas.symm, hdue, hlive, hadvance, rfl⟩

theorem renewal_before_due_is_rejected
    (pre : World Generation) (generation : Generation) (duration deadline : Time)
    (hlease : pre.lease = .active generation duration deadline)
    (hearly : pre.now < renewalDue duration deadline) :
    step? pre (.renew .mutationWriteGate generation deadline) = none := by
  simp [step?, hlease, admitted, Nat.not_le_of_gt hearly]

theorem renewal_stale_deadline_cas_is_rejected
    (pre : World Generation) (generation : Generation)
    (duration deadline staleDeadline : Time)
    (hlease : pre.lease = .active generation duration deadline)
    (hstale : staleDeadline ≠ deadline) :
    step? pre (.renew .mutationWriteGate generation staleDeadline) = none := by
  simp [step?, hlease, hstale.symm]

theorem duration_one_renewal_cannot_advance
    (pre : World Generation) (generation : Generation) (deadline : Time)
    (hlease : pre.lease = .active generation 1 deadline) :
    step? pre (.renew .mutationWriteGate generation deadline) = none := by
  by_cases hlive : pre.now < deadline
  · have hbound : pre.now + 1 ≤ deadline := hlive
    simp [step?, hlease, admitted, renewDeadline, hlive, Nat.not_lt_of_ge hbound]
  · simp [step?, hlease, admitted, hlive]

theorem incoherent_terminal_request_cannot_renew
    (pre : World Generation) (generation : Generation)
    (duration deadline : Time) (outcome : Outcome)
    (hlease : pre.lease = .active generation duration deadline)
    (hrequest : pre.request = outcome.requestState) :
    step? pre (.renew .mutationWriteGate generation deadline) = none := by
  cases outcome <;> simp_all [step?, hlease, Outcome.requestState, renewableLifecycle]

/-- Two successful renewals of one fixed-duration lease are cadence-separated.
The premise explicitly advances only the owner's monotonic clock; it is not a
scheduler-liveness claim. -/
theorem consecutive_renewals_obey_cadence
    (firstPre firstPost secondPre secondPost : World Generation)
    (generation : Generation) (duration firstDeadline secondTime : Time)
    (hfirstLease : firstPre.lease = .active generation duration firstDeadline)
    (hfirst : step? firstPre (.renew .mutationWriteGate generation firstDeadline) =
      some firstPost)
    (hsecondPre : secondPre = { firstPost with now := secondTime })
    (hsecond : step? secondPre
      (.renew .mutationWriteGate generation (firstPre.now + duration)) = some secondPost) :
    renewalDue duration (firstPre.now + duration) ≤ secondTime := by
  have hfirstResult := renewal_success_is_due_exact_cas_and_advances
    firstPre firstPost generation duration firstDeadline firstDeadline hfirstLease hfirst
  have hsecondLease : secondPre.lease =
      .active generation duration (firstPre.now + duration) := by
    simp [hsecondPre, hfirstResult.2.2.2.2]
  have hsecondResult := renewal_success_is_due_exact_cas_and_advances
    secondPre secondPost generation duration (firstPre.now + duration)
      (firstPre.now + duration) hsecondLease hsecond
  have hdue := hsecondResult.2.1
  rw [hsecondPre] at hdue
  simpa using hdue

theorem expired_recovery_is_atomic_generation_swap
    (pre post : World Generation) (expected generation : Generation)
    (oldDuration oldDeadline duration deadline : Time)
    (hlease : pre.lease = .active expected oldDuration oldDeadline)
    (h : step? pre
      (.recoverExpired .mutationWriteGate expected generation duration deadline) = some post) :
    oldDeadline ≤ pre.now ∧ fresh pre generation ∧
      post.lease = .active generation duration deadline ∧
      generation ∈ post.usedGenerations := by
  simp [step?, hlease, effectiveExpiry] at h
  rcases h with ⟨hguard, rfl⟩
  exact ⟨hguard.1, hguard.2.2.1, rfl, by simp [installFresh]⟩

theorem expired_recovery_enabled
    (pre : World Generation) (expected generation : Generation)
    (oldDuration oldDeadline duration deadline : Time)
    (hlease : pre.lease = .active expected oldDuration oldDeadline)
    (hexpired : oldDeadline ≤ pre.now) (hduration : duration > 0)
    (hfresh : fresh pre generation) (hdeadline : pre.now < deadline) :
    ∃ post, step? pre
      (.recoverExpired .mutationWriteGate expected generation duration deadline) = some post := by
  refine ⟨installFresh pre generation duration deadline, ?_⟩
  simp [step?, hlease, effectiveExpiry, hexpired, hduration, hfresh, hdeadline]

theorem observer_cannot_recover_expired
    (pre : World Generation) (expected generation : Generation) (duration deadline : Time) :
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
      have hadmit : owner = generation ∧ pre.now < deadline := by
        simpa [admitted, hlease] using hguard.1
      rw [hadmit.1]
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
      exact ⟨hguard.2.2.1, rfl, by simp [installFresh]⟩
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
