import Proofs.MCPHealth.Transition

/-!
# MCP Health / Eviction — Properties

Safety, arithmetic, and liveness facts about `step?` / `run?`. Property
tags (H1–H8, H6') match the spec table in §8 of
`docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md` (removed from the tree; see git history).

This file is built additively. Task 4 adds the easy safety/arithmetic facts;
later tasks add H7 (K=1 collapse), H6 / H6' (liveness), and H5 (anti-flapping
inter-eviction gap, load-bearing).
-/

namespace Proofs.MCPHealth

/-- H1: every legal next-state arises from a named `Event`; no spontaneous
    transitions. Trivially structural — `step?` is a total function of
    `Event`. Recorded as a fact for the audit's "no spontaneous transitions"
    acceptance criterion. -/
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

/-- H2: `probeSuccess` resets `failureCount` to 0. -/
@[simp]
theorem h2_success_resets_failure_count
    (sm : ServiceModel) (stale : Bool) (K : Threshold) :
    (step? sm (.probeSuccess stale) K).map (·.failureCount) = some 0 := rfl

/-- H3: `probeFail` increments `failureCount` by exactly 1. -/
@[simp]
theorem h3_probefail_increments_count
    (sm : ServiceModel) (K : Threshold) :
    (step? sm .probeFail K).map (·.failureCount) = some (sm.failureCount + 1) := by
  simp only [step?]
  split <;> simp

/-- H4: `backoffExpiry` only changes state when starting from `.evicted`. -/
@[simp]
theorem h4_backoff_only_from_evicted (sm : ServiceModel) (K : Threshold) :
    (step? sm .backoffExpiry K).map (·.state)
      = some (if sm.state = .evicted then .reconnecting else sm.state) := rfl

/-- H8: `registryAbsent` ends the per-service state machine. -/
@[simp]
theorem h8_registry_absent_terminates
    (sm : ServiceModel) (K : Threshold) :
    step? sm .registryAbsent K = none := rfl

/-- H7: at K=1, `probeFail` from any non-removed state with `failureCount = 0`
    goes directly to `.evicted`. Witnesses the K=1 collapse to today's Rust
    single-failure eviction. -/
theorem h7_k1_collapse_probefail_skips_degraded
    (sm : ServiceModel) (h0 : sm.failureCount = 0)
    (K : Threshold) (hk : K.val = 1) :
    (step? sm .probeFail K).map (·.state) = some .evicted := by
  simp [step?, h0, hk]

/-- Helper: when `step?` lands in `.degraded` via `probeFail`, the new
    `failureCount` is strictly less than K. This is the bookkeeping invariant
    that supports H5's induction in Task 7. -/
theorem degraded_count_lt_K
    (sm sm' : ServiceModel) (K : Threshold)
    (h : step? sm .probeFail K = some sm')
    (hd : sm'.state = .degraded) :
    sm'.failureCount < K.val := by
  simp only [step?] at h
  split at h
  · -- ≥ K branch — state becomes .evicted, contradicts hd = .degraded
    rename_i hge
    cases h
    exact absurd hd (by simp)
  · -- < K branch — state becomes .degraded, failureCount = sm.failureCount + 1 < K
    rename_i hlt
    cases h
    simp only [ServiceModel.mk.injEq] at *
    omega

/-- H6: from `.evicted`, the two-event sequence
    `[backoffExpiry, probeSuccess false]` reaches `.healthy`.
    Constructive liveness witness for the backoff-then-probe recovery path
    (relevant under K ≥ 2 with an armed backoff). -/
theorem h6_evicted_recovers_via_backoff_then_probe
    (sm : ServiceModel) (K : Threshold) (h : sm.state = .evicted) :
    (run? sm [.backoffExpiry, .probeSuccess false] K).map (·.state) = some .healthy := by
  simp [run?, List.foldl, Option.bind, step?, h]

/-- H6': from `.evicted`, a single `probeSuccess false` reaches `.healthy`
    directly (skipping `.reconnecting`). This is the **permissive** recovery
    path — `Reconnecting` is an optional pass-through state, not mandatory.

    Required by the K=1 conformance: today's Rust has no observable
    `Reconnecting` state, so a successful probe after eviction must assign
    `Healthy` directly. See spec §7.1 for the design rationale. -/
theorem h6'_evicted_recovers_via_probe_directly
    (sm : ServiceModel) (K : Threshold) (_h : sm.state = .evicted) :
    (step? sm (.probeSuccess false) K).map (·.state) = some .healthy := rfl

/-- Helper 1: across any successful `run?`, the gain in `failureCount` is
    bounded above by the number of `probeFail` events. Each `probeFail`
    increments `failureCount` by 1; `probeSuccess` resets to 0; `backoffExpiry`
    preserves; `registryAbsent` aborts the run. -/
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
          -- step? sm (.probeSuccess stale) K = some { sm with state := ..., failureCount := 0 }
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
          -- Either branch of step? yields some sm'' with failureCount = sm.failureCount + 1.
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

/-- Helper 2: if `run?` lands in `.evicted` and the invariant
    `(sm.state = .evicted → sm.failureCount ≥ K.val)` holds at the start,
    then the final `failureCount ≥ K.val`. The invariant is preserved by
    every legal transition in `step?`. -/
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
          -- New state: .healthy or .degraded; new failureCount: 0.
          have hstep : step? sm (.probeSuccess stale) K =
              some { state := if stale then .degraded else .healthy, failureCount := 0 } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          apply ih _ hrun
          intro hev
          -- new state is .healthy or .degraded, never .evicted; hev is impossible.
          cases stale <;> simp at hev
      | probeFail =>
          simp only [step?] at hrun
          split at hrun
          · -- ≥ K branch: new state = .evicted, new failureCount = sm.failureCount + 1 ≥ K
            rename_i hge
            simp [Option.bind] at hrun
            apply ih _ hrun
            intro _hev
            -- new failureCount = sm.failureCount + 1 ≥ K
            exact hge
          · -- < K branch: new state = .degraded; for it to remain .evicted later, invariant must apply
            rename_i _hlt
            simp [Option.bind] at hrun
            apply ih _ hrun
            intro hev
            -- new state is .degraded, hev says .degraded = .evicted, impossible
            simp at hev
      | backoffExpiry =>
          have hstep : step? sm .backoffExpiry K =
              some { sm with state := if sm.state = .evicted then .reconnecting else sm.state } := by
            simp [step?]
          rw [hstep] at hrun
          simp [Option.bind] at hrun
          apply ih _ hrun
          intro hev
          -- new state: .reconnecting if sm.state = .evicted, else sm.state. Neither is .evicted
          -- when entering from a state where the invariant held (we need .evicted at the new sm).
          by_cases hse : sm.state = .evicted
          · simp [hse] at hev  -- new state = .reconnecting, hev: .reconnecting = .evicted is false
          · -- new state = sm.state, hev: sm.state = .evicted, contradicts hse; simp closes the goal
            simp [hse] at hev
      | registryAbsent =>
          simp [step?] at hrun

/-- Helper 3: if `run?` lands in `.healthy` and the invariant
    `(sm.state = .healthy → sm.failureCount = 0)` holds at the start,
    then the final `failureCount = 0`. The invariant is preserved by every
    legal transition: `probeSuccess false` sets state to `.healthy` and count
    to 0 (preserved); `probeSuccess true` exits `.healthy`; `probeFail`
    exits `.healthy`; `backoffExpiry` from non-`.evicted` preserves state and
    count, so the invariant carries. -/
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
            -- new state is .evicted or .degraded, not .healthy; hh is impossible
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
          · simp [hse] at hh  -- new state = .reconnecting, hh: .reconnecting = .healthy, false
          · simp [hse] at hh  -- new state = sm.state, hh: sm.state = .healthy
            -- new failureCount = sm.failureCount, and by hinv applied to hh, = 0
            exact hinv hh
      | registryAbsent =>
          simp [step?] at hrun

/-- List arithmetic: for `p1 ≤ p2`, the prefix of length `p2` decomposes as
    the prefix of length `p1` appended with the next `p2 - p1` events. Direct
    consequence of `List.take_add`. Used to slice a run at an intermediate
    prefix in H5. -/
theorem take_split_at {α : Type} (l : List α) (p1 p2 : Nat) (h12 : p1 ≤ p2) :
    l.take p2 = l.take p1 ++ (l.drop p1).take (p2 - p1) := by
  have h : p1 + (p2 - p1) = p2 := by omega
  have := List.take_add l p1 (p2 - p1)
  rw [h] at this
  exact this

/-- H5: load-bearing anti-flapping safety property. If the run reaches
    `.healthy` at prefix `p1` and `.evicted` at later prefix `p2`, the events
    between the two prefixes contain at least `K` `probeFail` events.

    Proof: from the healthy prefix, the model has `failureCount = 0`. To reach
    `.evicted` at `p2`, the slice must drive `failureCount ≥ K`. Each
    `probeFail` raises `failureCount` by 1; nothing else does. So the slice
    contains ≥ K `probeFail` events. -/
theorem h5_anti_flapping_inter_eviction_gap
    (events : List Event) (K : Threshold)
    (p1 p2 : Nat) (h12 : p1 < p2) (_h2le : p2 ≤ events.length)
    (h1 : (run? ServiceModel.initial (events.take p1) K).map (·.state) = some .healthy)
    (h2 : (run? ServiceModel.initial (events.take p2) K).map (·.state) = some .evicted) :
    K.val ≤ ((events.drop p1).take (p2 - p1)).countP (· = Event.probeFail) := by
  -- Extract sm1 from h1.
  rcases hsm1 : run? ServiceModel.initial (events.take p1) K with _ | sm1
  · rw [hsm1] at h1; simp at h1
  rw [hsm1] at h1
  simp at h1
  -- h1 : sm1.state = .healthy
  -- Extract sm2 from h2.
  rcases hsm2 : run? ServiceModel.initial (events.take p2) K with _ | sm2
  · rw [hsm2] at h2; simp at h2
  rw [hsm2] at h2
  simp at h2
  -- h2 : sm2.state = .evicted
  -- Decompose events.take p2 = events.take p1 ++ (events.drop p1).take (p2 - p1)
  have hp12 : p1 ≤ p2 := Nat.le_of_lt h12
  have hsplit : events.take p2 = events.take p1 ++ (events.drop p1).take (p2 - p1) :=
    take_split_at events p1 p2 hp12
  -- Apply run?_append: run? init (a ++ b) K = (run? init a K).bind (run? · b K)
  rw [hsplit, run?_append] at hsm2
  rw [hsm1] at hsm2
  simp [Option.bind] at hsm2
  -- hsm2 : run? sm1 ((events.drop p1).take (p2 - p1)) K = some sm2
  -- Get sm1.failureCount = 0 from Helper 3.
  have h_init_inv : ServiceModel.initial.state = .healthy →
      ServiceModel.initial.failureCount = 0 := fun _ => rfl
  have hsm1_fc : sm1.failureCount = 0 := by
    -- Apply Helper 3 to the initial run.
    have := healthy_failureCount_eq_zero ServiceModel.initial sm1 (events.take p1) K hsm1 h1
              h_init_inv
    exact this
  -- Get sm2.failureCount ≥ K.val from Helper 2.
  have hsm2_fc_ge : sm2.failureCount ≥ K.val := by
    apply evicted_failureCount_ge_K sm1 sm2 ((events.drop p1).take (p2 - p1)) K hsm2 h2
    intro hev
    -- sm1.state = .evicted, but sm1.state = .healthy, contradiction.
    rw [h1] at hev
    cases hev
  -- Get sm2.failureCount ≤ sm1.failureCount + slice probefail count from Helper 1.
  have hbound :=
    failureCount_le_probefail_count sm1 sm2 ((events.drop p1).take (p2 - p1)) K hsm2
  rw [hsm1_fc] at hbound
  -- Combine: K ≤ sm2.failureCount ≤ 0 + slice count
  omega

end Proofs.MCPHealth
