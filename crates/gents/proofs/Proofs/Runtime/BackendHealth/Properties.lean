import Proofs.BackendHealth.Transition

namespace Proofs.BackendHealth

@[simp]
theorem probefail_increments_count (m : Model) (K : Threshold) :
    (step m .probeFail K).failureCount = m.failureCount + 1 := by
  simp only [step]
  split
  · rfl
  · split <;> rfl

theorem probefail_preserves_account_recovery (m : Model) (K : Threshold) :
    (step m .probeFail K).state.needsAccountRecovery = m.state.needsAccountRecovery := by
  simp only [step]
  split
  · rfl
  · split <;> simp_all [HealthState.needsAccountRecovery]

theorem run_replicate_probeFail (m : Model) (hm : m.state.needsAccountRecovery = false)
    (j : Nat) (K : Threshold) :
    run m (List.replicate (j + 1) .probeFail) K =
      { state := if K.val ≤ m.failureCount + (j + 1) then .unhealthy else .degraded
      , failureCount := m.failureCount + (j + 1) } := by
  induction j generalizing m with
  | zero =>
      have hone : List.replicate (0 + 1) Event.probeFail = [Event.probeFail] := by simp
      rw [hone]
      show step m .probeFail K = _
      simp only [step, hm, Bool.false_eq_true, if_false, ge_iff_le, Nat.zero_add]
      split <;> rename_i h <;> simp [h]
  | succ j ih =>
      rw [List.replicate_succ, run_cons]
      have hnext : (step m .probeFail K).state.needsAccountRecovery = false := by
        rw [probefail_preserves_account_recovery, hm]
      rw [ih (step m .probeFail K) hnext, probefail_increments_count]
      have harith : m.failureCount + 1 + (j + 1) = m.failureCount + (j + 1 + 1) := by omega
      rw [harith]

theorem b1_demotes_at_K (m : Model) (hm : m.state.needsAccountRecovery = false)
    (h0 : m.failureCount = 0) (K : Threshold) :
    (run m (List.replicate K.val .probeFail) K).state = .unhealthy := by
  obtain ⟨j, hj⟩ : ∃ j, K.val = j + 1 := ⟨K.val - 1, by omega⟩
  rw [hj, run_replicate_probeFail m hm, h0]
  simp [hj]

theorem b2_no_demote_below_K
    (m : Model) (h0 : m.failureCount = 0) (hm : m.state.blocksRouting = false)
    (n : Nat) (K : Threshold) (hn : n < K.val) :
    (run m (List.replicate n .probeFail) K).state.blocksRouting = false := by
  have hordinary : m.state.needsAccountRecovery = false := by
    cases h : m.state <;> simp_all [HealthState.blocksRouting, HealthState.needsAccountRecovery]
  cases n with
  | zero => simpa using hm
  | succ j =>
      rw [run_replicate_probeFail m hordinary, h0]
      have hlt : ¬ K.val ≤ j + 1 := by omega
      simp [Nat.zero_add, hlt, HealthState.blocksRouting]

theorem b3_single_success_promotes (m : Model) (K : Threshold) :
    step m .probeSuccess K = { state := .healthy, failureCount := 0 } := rfl

theorem b4_intent_never_overridden (m : Model) :
    effectiveAvailable false m = false := rfl

theorem b5_unknown_does_not_block :
    HealthState.blocksRouting .unknown = false := rfl

theorem b6_effectiveAvailable_iff (intent : Bool) (m : Model) :
    effectiveAvailable intent m = true ↔ intent = true ∧ m.state.blocksRouting = false := by
  cases intent <;>
    cases h : m.state <;>
      simp [effectiveAvailable, HealthState.blocksRouting, h]

theorem rejected_credentials_block_immediately (m : Model) (K : Threshold) :
    (step m .credentialsRejected K).state.blocksRouting = true := rfl

theorem rejected_entitlement_blocks_immediately (m : Model) (K : Threshold) :
    (step m .subscriptionNotEntitled K).state.blocksRouting = true := rfl

theorem network_failure_preserves_account_failure (m : Model) (K : Threshold)
    (hm : m.state.needsAccountRecovery = true) :
    (step m .probeFail K).state = m.state := by
  simp [step, hm]

/-- A measurement belongs to its connection configuration. Selecting another
account or endpoint retires the old account failure before probing it. -/
theorem configuration_change_retires_old_measurement (m : Model) (K : Threshold) :
    step m .configurationChanged K = Model.initial := rfl

theorem unhealthy_only_via_threshold (m m' : Model) (e : Event) (K : Threshold)
    (h : step m e K = m') (hu : m'.state = .unhealthy) :
    e = .probeFail ∧ K.val ≤ m'.failureCount := by
  cases e with
  | probeSuccess =>
      subst h
      exact absurd hu (by simp [step])
  | probeFail =>
      refine ⟨rfl, ?_⟩
      simp only [step] at h
      split at h
      · rename_i recovery
        subst h
        simp_all [HealthState.needsAccountRecovery]
      · split at h
        · rename_i hge
          subst h
          simpa using hge
        · subst h
          exact absurd hu (by simp)
  | credentialsRejected | subscriptionNotEntitled | configurationChanged =>
      subst h
      simp [step, Model.initial] at hu

end Proofs.BackendHealth
