import Proofs.BackendHealth.Transition

/-!
# Backend Health — Properties

The #640 contract: K consecutive failures demote (and only K — no flap
below the threshold), one success promotes, operator intent is never
overridden upward, and the unprobed state never vetoes routing.

Property tags B1–B6 match §1 of
`docs/superpowers/specs/2026-07-07-backend-probe-health-640-design.md`.
-/

namespace Proofs.BackendHealth

/-- Workhorse: `j + 1` consecutive `probeFail`s from any model land on
    `failureCount + (j + 1)` failures, `.unhealthy` iff that count reaches K
    and `.degraded` otherwise. -/
theorem run_replicate_probeFail (m : Model) (j : Nat) (K : Threshold) :
    run m (List.replicate (j + 1) .probeFail) K =
      { state := if K.val ≤ m.failureCount + (j + 1) then .unhealthy else .degraded
      , failureCount := m.failureCount + (j + 1) } := by
  induction j generalizing m with
  | zero =>
      have hone : List.replicate (0 + 1) Event.probeFail = [Event.probeFail] := by simp
      rw [hone]
      show step m .probeFail K = _
      simp only [step, ge_iff_le, Nat.zero_add]
      split <;> rename_i h <;> simp [h]
  | succ j ih =>
      rw [List.replicate_succ, run_cons]
      -- step m .probeFail K has failureCount = m.failureCount + 1 in both branches.
      have hstep : (step m .probeFail K).failureCount = m.failureCount + 1 := by
        simp only [step]
        split <;> rfl
      rw [ih (step m .probeFail K), hstep]
      have harith : m.failureCount + 1 + (j + 1) = m.failureCount + (j + 1 + 1) := by omega
      rw [harith]

/-- B1 (demotion at K): from a clean counter, exactly `K` consecutive probe
    failures measure the backend `.unhealthy`. -/
theorem b1_demotes_at_K (m : Model) (h0 : m.failureCount = 0) (K : Threshold) :
    (run m (List.replicate K.val .probeFail) K).state = .unhealthy := by
  obtain ⟨j, hj⟩ : ∃ j, K.val = j + 1 := ⟨K.val - 1, by omega⟩
  rw [hj, run_replicate_probeFail, h0]
  simp [hj]

/-- B2 (no flap below K): fewer than `K` consecutive failures never veto
    routing — a blip cannot demote. `hm` covers the vacuous `n = 0` case. -/
theorem b2_no_demote_below_K
    (m : Model) (h0 : m.failureCount = 0) (hm : m.state.blocksRouting = false)
    (n : Nat) (K : Threshold) (hn : n < K.val) :
    (run m (List.replicate n .probeFail) K).state.blocksRouting = false := by
  cases n with
  | zero => simpa using hm
  | succ j =>
      rw [run_replicate_probeFail, h0]
      have hlt : ¬ K.val ≤ j + 1 := by omega
      simp [Nat.zero_add, hlt, HealthState.blocksRouting]

/-- B3 (single-success promotion): one successful probe promotes to
    `.healthy` with a clean counter, from ANY prior state — including
    `.unhealthy`. Routing resumes after one good probe. -/
theorem b3_single_success_promotes (m : Model) (K : Threshold) :
    step m .probeSuccess K = { state := .healthy, failureCount := 0 } := rfl

/-- B4 (intent is a hard gate): measured health never resurrects a backend
    the operator/bootstrap intent has not made available. -/
theorem b4_intent_never_overridden (m : Model) :
    effectiveAvailable false m = false := rfl

/-- B5 (startup grace): the never-probed state does not veto routing —
    doc intent alone governs until the first cycle completes. -/
theorem b5_unknown_does_not_block :
    HealthState.blocksRouting .unknown = false := rfl

/-- B6 (availability projection soundness): the effective gate holds exactly
    when intent holds and the measurement is not `.unhealthy`. This is the
    contract `BackendAdmissionConfig::is_available` mirrors. -/
theorem b6_effectiveAvailable_iff (intent : Bool) (m : Model) :
    effectiveAvailable intent m = true ↔ intent = true ∧ m.state ≠ .unhealthy := by
  cases intent <;>
    cases h : m.state <;>
      simp [effectiveAvailable, HealthState.blocksRouting, h]

/-- Bookkeeping: `probeFail` increments the counter by exactly 1. -/
@[simp]
theorem probefail_increments_count (m : Model) (K : Threshold) :
    (step m .probeFail K).failureCount = m.failureCount + 1 := by
  simp only [step]
  split <;> rfl

/-- Bookkeeping: `.unhealthy` is entered only via `probeFail` at count ≥ K. -/
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
      · rename_i hge
        subst h
        simpa using hge
      · subst h
        exact absurd hu (by simp)

end Proofs.BackendHealth
