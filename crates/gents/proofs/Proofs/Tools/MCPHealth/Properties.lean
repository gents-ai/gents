import Proofs.MCPHealth.Transition

namespace Proofs.MCPHealth

theorem h1_event_triggered
    (sm sm' : ServiceModel) (K : Threshold) :
    (∃ e, step? sm e K = some sm') ↔
      sm' ∈ (Event.all.filterMap (fun e => step? sm e K)) := by
  constructor
  · rintro ⟨e, he⟩
    have := Event.all_complete e
    simp [List.mem_filterMap]
    exact ⟨e, this, he⟩
  · intro h
    simp [List.mem_filterMap] at h
    obtain ⟨e, _, he⟩ := h
    exact ⟨e, he⟩

@[simp]
theorem h2_success_resets_failure_count
    (sm : ServiceModel) (stale : Bool) (K : Threshold) :
    (step? sm (.probeSuccess stale) K).map (·.failureCount) = some 0 := rfl

@[simp]
theorem h3_probefail_increments_count
    (sm : ServiceModel) (K : Threshold) :
    (step? sm .probeFail K).map (·.failureCount) = some (sm.failureCount + 1) := by
  simp only [step?]
  split <;> simp

@[simp]
theorem h4_backoff_only_from_evicted (sm : ServiceModel) (K : Threshold) :
    (step? sm .backoffExpiry K).map (·.state)
      = some (if sm.state = .evicted then .reconnecting else sm.state) := rfl

@[simp]
theorem h8_registry_absent_terminates
    (sm : ServiceModel) (K : Threshold) :
    step? sm .registryAbsent K = none := rfl

theorem h7_k1_collapse_probefail_skips_degraded
    (sm : ServiceModel) (h0 : sm.failureCount = 0)
    (K : Threshold) (hk : K.val = 1) :
    (step? sm .probeFail K).map (·.state) = some .evicted := by
  simp [step?, h0, hk]

theorem degraded_count_lt_K
    (sm sm' : ServiceModel) (K : Threshold)
    (h : step? sm .probeFail K = some sm')
    (hd : sm'.state = .degraded) :
    sm'.failureCount < K.val := by
  simp only [step?] at h
  split at h
  ·
    rename_i hge
    cases h
    exact absurd hd (by simp)
  ·
    rename_i hlt
    cases h
    simp only [ServiceModel.mk.injEq] at *
    omega

theorem h6_evicted_recovers_via_backoff_then_probe
    (sm : ServiceModel) (K : Threshold) (h : sm.state = .evicted) :
    (run? sm [.backoffExpiry, .probeSuccess false] K).map (·.state) = some .healthy := by
  simp [run?, List.foldl, Option.bind, step?, h]

theorem h6'_evicted_recovers_via_probe_directly
    (sm : ServiceModel) (K : Threshold) (_h : sm.state = .evicted) :
    (step? sm (.probeSuccess false) K).map (·.state) = some .healthy := rfl

theorem failureCount_le_probefail_count
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm') :
    sm'.failureCount ≤ sm.failureCount + events.countP (· = Event.probeFail) := by
  induction events generalizing sm with
  | nil =>
      simp [run?] at hrun
      subst hrun
      simp
  | cons e rest ih =>
      rw [run?_cons] at hrun
      cases e with
      | probeSuccess stale =>
          have hstep : step? sm (.probeSuccess stale) K =
              some { state := if stale then .degraded else .healthy, failureCount := 0 } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          have hih := ih _ hrun
          have hne : ¬ ((Event.probeSuccess stale) = Event.probeFail) := by
            intro h; cases h
          rw [List.countP_cons_of_neg _ _ (by simp [hne])]
          simp at hih
          omega
      | probeFail =>
          have hstep : ∃ sm'', step? sm .probeFail K = some sm'' ∧
              sm''.failureCount = sm.failureCount + 1 := by
            simp only [step?]
            split
            · refine ⟨_, rfl, ?_⟩; simp
            · refine ⟨_, rfl, ?_⟩; simp
          obtain ⟨sm'', hstep_eq, hfc⟩ := hstep
          rw [hstep_eq] at hrun
          simp [Option.bind] at hrun
          have hih := ih _ hrun
          rw [List.countP_cons_of_pos _ _ (by simp)]
          rw [hfc] at hih
          omega
      | backoffExpiry =>
          have hstep : step? sm .backoffExpiry K =
              some { sm with state := if sm.state = .evicted then .reconnecting else sm.state } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          have hih := ih _ hrun
          have hne : ¬ ((Event.backoffExpiry : Event) = Event.probeFail) := by
            intro h; cases h
          rw [List.countP_cons_of_neg _ _ (by simp [hne])]
          simp at hih
          omega
      | registryAbsent =>
          simp [step?] at hrun

theorem evicted_failureCount_ge_K
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm')
    (hst : sm'.state = .evicted)
    (hinv : sm.state = .evicted → sm.failureCount ≥ K.val) :
    sm'.failureCount ≥ K.val := by
  induction events generalizing sm with
  | nil =>
      simp [run?] at hrun
      subst hrun
      exact hinv hst
  | cons e rest ih =>
      rw [run?_cons] at hrun
      cases e with
      | probeSuccess stale =>
          have hstep : step? sm (.probeSuccess stale) K =
              some { state := if stale then .degraded else .healthy, failureCount := 0 } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          apply ih _ hrun
          intro hev
          cases stale <;> simp at hev
      | probeFail =>
          simp only [step?] at hrun
          split at hrun
          ·
            rename_i hge
            simp [Option.bind] at hrun
            apply ih _ hrun
            intro _hev
            exact hge
          ·
            rename_i _hlt
            simp [Option.bind] at hrun
            apply ih _ hrun
            intro hev
            simp at hev
      | backoffExpiry =>
          have hstep : step? sm .backoffExpiry K =
              some { sm with state := if sm.state = .evicted then .reconnecting else sm.state } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          apply ih _ hrun
          intro hev
          by_cases hse : sm.state = .evicted
          · simp [hse] at hev
          ·
            simp [hse] at hev
      | registryAbsent =>
          simp [step?] at hrun

theorem healthy_failureCount_eq_zero
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm')
    (hst : sm'.state = .healthy)
    (hinv : sm.state = .healthy → sm.failureCount = 0) :
    sm'.failureCount = 0 := by
  induction events generalizing sm with
  | nil =>
      simp [run?] at hrun
      subst hrun
      exact hinv hst
  | cons e rest ih =>
      rw [run?_cons] at hrun
      cases e with
      | probeSuccess stale =>
          have hstep : step? sm (.probeSuccess stale) K =
              some { state := if stale then .degraded else .healthy, failureCount := 0 } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          apply ih _ hrun
          intro _hh
          rfl
      | probeFail =>
          simp only [step?] at hrun
          split at hrun
          all_goals {
            simp [Option.bind] at hrun
            apply ih _ hrun
            intro hh
            simp at hh
          }
      | backoffExpiry =>
          have hstep : step? sm .backoffExpiry K =
              some { sm with state := if sm.state = .evicted then .reconnecting else sm.state } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          apply ih _ hrun
          intro hh
          by_cases hse : sm.state = .evicted
          · simp [hse] at hh
          · simp [hse] at hh
            exact hinv hh
      | registryAbsent =>
          simp [step?] at hrun

theorem take_split_at {α : Type} (l : List α) (p1 p2 : Nat) (h12 : p1 ≤ p2) :
    l.take p2 = l.take p1 ++ (l.drop p1).take (p2 - p1) := by
  have h : p1 + (p2 - p1) = p2 := by omega
  have := List.take_add l p1 (p2 - p1)
  rw [h] at this
  exact this

theorem h5_anti_flapping_inter_eviction_gap
    (events : List Event) (K : Threshold)
    (p1 p2 : Nat) (h12 : p1 < p2) (_h2le : p2 ≤ events.length)
    (h1 : (run? ServiceModel.initial (events.take p1) K).map (·.state) = some .healthy)
    (h2 : (run? ServiceModel.initial (events.take p2) K).map (·.state) = some .evicted) :
    K.val ≤ ((events.drop p1).take (p2 - p1)).countP (· = Event.probeFail) := by
  rcases hsm1 : run? ServiceModel.initial (events.take p1) K with _ | sm1
  · rw [hsm1] at h1; simp at h1
  rw [hsm1] at h1
  simp at h1
  rcases hsm2 : run? ServiceModel.initial (events.take p2) K with _ | sm2
  · rw [hsm2] at h2; simp at h2
  rw [hsm2] at h2
  simp at h2
  have hp12 : p1 ≤ p2 := Nat.le_of_lt h12
  have hsplit : events.take p2 = events.take p1 ++ (events.drop p1).take (p2 - p1) :=
    take_split_at events p1 p2 hp12
  rw [hsplit, run?_append] at hsm2
  rw [hsm1] at hsm2
  simp [Option.bind] at hsm2
  have h_init_inv : ServiceModel.initial.state = .healthy →
      ServiceModel.initial.failureCount = 0 := fun _ => rfl
  have hsm1_fc : sm1.failureCount = 0 := by
    have := healthy_failureCount_eq_zero ServiceModel.initial sm1 (events.take p1) K hsm1 h1
              h_init_inv
    exact this
  have hsm2_fc_ge : sm2.failureCount ≥ K.val := by
    apply evicted_failureCount_ge_K sm1 sm2 ((events.drop p1).take (p2 - p1)) K hsm2 h2
    intro hev
    rw [h1] at hev
    cases hev
  have hbound :=
    failureCount_le_probefail_count sm1 sm2 ((events.drop p1).take (p2 - p1)) K hsm2
  rw [hsm1_fc] at hbound
  omega

end Proofs.MCPHealth
