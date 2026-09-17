import Proofs.SelfConfig.Apply

namespace SelfConfig

theorem applyEntry_protected (t : Target) (doc : Doc) (e : PatchEntry)
    (k : FieldKey) (hk : k ∉ writableFields t) :
    applyEntry t doc e k = doc k := by
  unfold applyEntry
  by_cases hw : e.key ∈ writableFields t
  · rw [if_pos hw]
    by_cases hke : k = e.key
    · exact absurd (hke ▸ hw) hk
    · simp [hke]
  · rw [if_neg hw]

theorem applyPatch_protected (t : Target) (doc : Doc) (p : Patch)
    (k : FieldKey) (hk : k ∉ writableFields t) :
    applyPatch t doc p k = doc k := by
  induction p generalizing doc with
  | nil => rfl
  | cons e rest ih =>
      show applyPatch t (applyEntry t doc e) rest k = doc k
      rw [ih (applyEntry t doc e)]
      exact applyEntry_protected t doc e k hk

theorem identity_immutable (t : Target) (doc : Doc) (p : Patch)
    (k : FieldKey) (hk : k ∈ protectedFields t) :
    applyPatch t doc p k = doc k := by
  apply applyPatch_protected
  have hmem := List.mem_filter.mp hk
  exact of_decide_eq_true hmem.2

theorem containment (t : Target) (doc : Doc) (p : Patch) (k : FieldKey)
    (h : applyPatch t doc p k ≠ doc k) :
    k ∈ writableFields t ∧ p.any (fun e => e.key == k) = true := by
  induction p generalizing doc with
  | nil => exact absurd rfl h
  | cons e rest ih =>
      have hcons : applyPatch t doc (e :: rest) k
          = applyPatch t (applyEntry t doc e) rest k := rfl
      by_cases he : applyEntry t doc e k = doc k
      · have hrest : applyPatch t (applyEntry t doc e) rest k
            ≠ applyEntry t doc e k := by
          rw [he]
          rw [hcons] at h
          exact h
        obtain ⟨hw, hp⟩ := ih (applyEntry t doc e) hrest
        refine ⟨hw, ?_⟩
        simp [List.any_cons, hp]
      · unfold applyEntry at he
        by_cases hw : e.key ∈ writableFields t
        · rw [if_pos hw] at he
          by_cases hke : k = e.key
          · subst hke
            refine ⟨hw, ?_⟩
            simp [List.any_cons]
          · simp [hke] at he
        · rw [if_neg hw] at he
          exact absurd rfl he

theorem step_accepts_wholesale (validate guard : Doc → Bool) (t : Target)
    (stored : Doc) (p : Patch) (merged : Doc)
    (h : step validate guard t stored p = some merged) :
    merged = applyPatch t stored p := by
  unfold step at h
  by_cases ha : admissible t p = true
  · rw [if_pos ha] at h
    by_cases hv : (validate (applyPatch t stored p)
        && guard (applyPatch t stored p)) = true
    · rw [if_pos hv] at h
      exact (Option.some.inj h).symm
    · rw [if_neg hv] at h
      exact Option.noConfusion h
  · rw [if_neg ha] at h
    exact Option.noConfusion h

theorem step_accept_validates (validate guard : Doc → Bool) (t : Target)
    (stored : Doc) (p : Patch) (merged : Doc)
    (h : step validate guard t stored p = some merged) :
    validate merged = true ∧ guard merged = true := by
  have hm := step_accepts_wholesale validate guard t stored p merged h
  unfold step at h
  by_cases ha : admissible t p = true
  · rw [if_pos ha] at h
    by_cases hv : (validate (applyPatch t stored p)
        && guard (applyPatch t stored p)) = true
    · rw [hm]
      simpa using hv
    · rw [if_neg hv] at h
      exact Option.noConfusion h
  · rw [if_neg ha] at h
    exact Option.noConfusion h

theorem step_inadmissible_rejects (validate guard : Doc → Bool) (t : Target)
    (stored : Doc) (p : Patch) (h : admissible t p = false) :
    step validate guard t stored p = none := by
  unfold step
  have hna : ¬(admissible t p = true) := by simp [h]
  rw [if_neg hna]

theorem runStep_reject_frame (validate guard : Doc → Bool) (t : Target)
    (s : Store) (p : Patch)
    (h : (runStep validate guard t s p).2 = false) :
    (runStep validate guard t s p).1 = s := by
  cases hstep : step validate guard t (s t) p with
  | none => simp [runStep, hstep]
  | some merged => simp [runStep, hstep] at h

theorem runStep_accept_frame (validate guard : Doc → Bool) (t : Target)
    (s : Store) (p : Patch) (t' : Target) (ht : t' ≠ t) :
    (runStep validate guard t s p).1 t' = s t' := by
  cases hstep : step validate guard t (s t) p with
  | none => simp [runStep, hstep]
  | some merged => simp [runStep, hstep, ht]

theorem runStep_accept_target (validate guard : Doc → Bool) (t : Target)
    (s : Store) (p : Patch)
    (h : (runStep validate guard t s p).2 = true) :
    (runStep validate guard t s p).1 t = applyPatch t (s t) p := by
  cases hstep : step validate guard t (s t) p with
  | none => simp [runStep, hstep] at h
  | some merged =>
      have hm := step_accepts_wholesale validate guard t (s t) p merged hstep
      simp [runStep, hstep, hm]

theorem no_lockout_recoverable (decodeEnabled : Doc → Option Bool) (validate : Doc → Bool) (s : Store) (p : Patch)
    (h : (runStep validate (gateOn decodeEnabled) .tools s p).2 = true) :
    (gateOn decodeEnabled) ((runStep validate (gateOn decodeEnabled) .tools s p).1 .tools)
      = true := by
  cases hstep : step validate (gateOn decodeEnabled) .tools (s .tools) p with
  | none => simp [runStep, hstep] at h
  | some merged =>
      have hval := step_accept_validates validate (gateOn decodeEnabled) .tools
        (s .tools) p merged hstep
      simp [runStep, hstep, hval.2]

theorem runStep_identity_immutable (validate guard : Doc → Bool) (t : Target)
    (s : Store) (p : Patch) (k : FieldKey) (hk : k ∈ protectedFields t) :
    (runStep validate guard t s p).1 t k = s t k := by
  cases hstep : step validate guard t (s t) p with
  | none => simp [runStep, hstep]
  | some merged =>
      have hm := step_accepts_wholesale validate guard t (s t) p merged hstep
      have himm := identity_immutable t (s t) p k hk
      simp [runStep, hstep, hm, himm]

theorem applyStorePatch_scope_immutable (s : Store) (p : StorePatch)
    (t : Target) (ht : t ∈ scopedTargets) :
    applyStorePatch s p t "scope_behavior_id" = s t "scope_behavior_id" := by
  have hall : t ∈ allTargets := scoped_targets_are_self_config_targets t ht
  simp only [applyStorePatch, if_pos hall]
  exact applyPatch_protected t (s t) (p t) "scope_behavior_id"
    (scope_behavior_id_never_writable t hall)

theorem runOwnerStep_accepts_valid_closure (validate guard : Store → Bool)
    (s : Store) (p : StorePatch)
    (accepted : (runOwnerStep validate guard s p).2 = true) :
    validate (runOwnerStep validate guard s p).1 = true := by
  by_cases h : (admissibleStorePatch p && validate (applyStorePatch s p) &&
      guard (applyStorePatch s p)) = true
  · have valid : validate (applyStorePatch s p) = true := by
      have parts : (admissibleStorePatch p = true ∧
          validate (applyStorePatch s p) = true) ∧
          guard (applyStorePatch s p) = true := by
        simpa only [Bool.and_eq_true] using h
      exact parts.1.2
    simpa [runOwnerStep, h] using valid
  · simp [runOwnerStep, h] at accepted

theorem runOwnerStep_rejects_atomically (validate guard : Store → Bool)
    (s : Store) (p : StorePatch)
    (rejected : (runOwnerStep validate guard s p).2 = false) :
    (runOwnerStep validate guard s p).1 = s := by
  by_cases h : (admissibleStorePatch p && validate (applyStorePatch s p) &&
      guard (applyStorePatch s p)) = true
  · simp [runOwnerStep, h] at rejected
  · simp [runOwnerStep, h]

/-- Full materialization and sparse edits have the same atomic outcome: the
complete validated candidate or the exact previous store. -/
theorem runOwnerStep_all_or_nothing (validate guard : Store → Bool)
    (s : Store) (p : StorePatch) :
    (runOwnerStep validate guard s p).1 = applyStorePatch s p ∨
      (runOwnerStep validate guard s p).1 = s := by
  by_cases h : (admissibleStorePatch p && validate (applyStorePatch s p) &&
      guard (applyStorePatch s p)) = true
  · exact Or.inl (by simp [runOwnerStep, h])
  · exact Or.inr (by simp [runOwnerStep, h])

theorem runOwnerStep_scope_immutable (validate guard : Store → Bool)
    (s : Store) (p : StorePatch) (t : Target) (ht : t ∈ scopedTargets) :
    (runOwnerStep validate guard s p).1 t "scope_behavior_id" =
      s t "scope_behavior_id" := by
  by_cases h : (admissibleStorePatch p && validate (applyStorePatch s p) &&
      guard (applyStorePatch s p)) = true
  · simpa [runOwnerStep, h] using applyStorePatch_scope_immutable s p t ht
  · simp [runOwnerStep, h]

theorem runScopedStep_accepts_valid_closure (validate guard : Store → Bool)
    (target : Target) (s : Store) (patch : Patch)
    (accepted : (runScopedStep validate guard target s patch).2 = true) :
    validate (runScopedStep validate guard target s patch).1 = true := by
  exact runOwnerStep_accepts_valid_closure validate guard s
    (sparseStorePatch target patch) accepted

theorem materializeClosure_accepts_valid (validate : Store → Bool)
    (stored candidate : Store)
    (accepted : (materializeClosure validate stored candidate).2 = true) :
    validate (materializeClosure validate stored candidate).1 = true := by
  by_cases h : validate candidate = true
  · simp [materializeClosure, h]
  · simp [materializeClosure, h] at accepted

theorem materializeClosure_rejects_atomically (validate : Store → Bool)
    (stored candidate : Store)
    (rejected : (materializeClosure validate stored candidate).2 = false) :
    (materializeClosure validate stored candidate).1 = stored := by
  by_cases h : validate candidate = true
  · simp [materializeClosure, h] at rejected
  · simp [materializeClosure, h]

theorem materializeClosure_all_or_nothing (validate : Store → Bool)
    (stored candidate : Store) :
    (materializeClosure validate stored candidate).1 = candidate ∨
      (materializeClosure validate stored candidate).1 = stored := by
  by_cases h : validate candidate = true
  · exact Or.inl (by simp [materializeClosure, h])
  · exact Or.inr (by simp [materializeClosure, h])

end SelfConfig
