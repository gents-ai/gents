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

theorem no_lockout_recoverable (validate : Doc → Bool) (s : Store) (p : Patch)
    (h : (runStep validate gateOn .toolSelection s p).2 = true) :
    gateOn ((runStep validate gateOn .toolSelection s p).1 .toolSelection)
      = true := by
  cases hstep : step validate gateOn .toolSelection (s .toolSelection) p with
  | none => simp [runStep, hstep] at h
  | some merged =>
      have hval := step_accept_validates validate gateOn .toolSelection
        (s .toolSelection) p merged hstep
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

end SelfConfig
