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

theorem no_lockout_recoverable (decode : Doc → Option Control) (validate : Doc → Bool)
    (s : Store) (p : Patch)
    (h : (runStep validate (keepsControl decode (s .tools)) .tools s p).2 = true) :
    ∃ old new, decode (s .tools) = some old ∧
      decode ((runStep validate (keepsControl decode (s .tools)) .tools s p).1 .tools)
        = some new ∧
      new.selfConfig = true ∧ (old.agents = true → new.agents = true) ∧
      (old.noLockout = true → new.noLockout = true) ∧
      (old.toolsAuthority = true → new.toolsAuthority = true) := by
  cases hstep : step validate (keepsControl decode (s .tools)) .tools (s .tools) p with
  | none => simp [runStep, hstep] at h
  | some merged =>
      have hg := (step_accept_validates validate (keepsControl decode (s .tools)) .tools
        (s .tools) p merged hstep).2
      have hpost :
          (runStep validate (keepsControl decode (s .tools)) .tools s p).1 .tools = merged := by
        simp [runStep, hstep]
      rw [hpost]
      unfold keepsControl at hg
      cases ho : decode (s .tools) with
      | none => simp [ho] at hg
      | some old =>
        cases hn : decode merged with
        | none => simp [ho, hn] at hg
        | some new =>
          rw [ho, hn] at hg
          simp only [retained, Bool.and_eq_true, Bool.or_eq_true, Bool.not_eq_true'] at hg
          obtain ⟨⟨⟨hself, hagents⟩, hguard⟩, htools⟩ := hg
          refine ⟨old, new, rfl, rfl, hself, ?_, ?_, ?_⟩
          · intro ha; cases hagents <;> simp_all
          · intro ha; cases hguard <;> simp_all
          · intro ha; cases htools <;> simp_all

/-- Turning the invoker's own self-config tool off is a lockout. -/
theorem self_config_disable_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (hoff : new.selfConfig = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, ho, hn, hoff]

/-- Removing an agents tool group the invoker already had is a lockout. -/
theorem agents_removal_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.agents = true) (removed : new.agents = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, retained, ho, hn, had, removed]

/-- Dropping the guard is the first step of a two-step lockout. -/
theorem no_lockout_removal_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.noLockout = true) (removed : new.noLockout = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, retained, ho, hn, had, removed]

/-- Dropping the `tools` category leaves self-config on but unable to restore. -/
theorem tools_authority_removal_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.toolsAuthority = true) (removed : new.toolsAuthority = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, retained, ho, hn, had, removed]

/-- Disabling the invoking behavior is a lockout. -/
theorem self_disable_refused (decode : Doc → Option Reach)
    (stored candidate : Doc) (old new : Reach)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (hoff : new.enabled = false) :
    keepsReach decode stored candidate = false := by
  simp [keepsReach, ho, hn, hoff]

/-- Dropping the Setup tag is the first step of a two-step self-disable. -/
theorem setup_tag_removal_refused (decode : Doc → Option Reach)
    (stored candidate : Doc) (old new : Reach)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.setupTag = true) (removed : new.setupTag = false) :
    keepsReach decode stored candidate = false := by
  simp [keepsReach, retained, ho, hn, had, removed]

/-- Everything else on the invoker's own Tools is allowed. -/
theorem retained_control_allowed (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (hon : new.selfConfig = true) (hagents : old.agents = true → new.agents = true)
    (hguard : old.noLockout = true → new.noLockout = true)
    (htools : old.toolsAuthority = true → new.toolsAuthority = true) :
    keepsControl decode stored candidate = true := by
  have r : ∀ a b : Bool, (a = true → b = true) → retained a b = true := by
    intro a b hab; cases a <;> cases b <;> simp_all [retained]
  simp [keepsControl, ho, hn, hon, r _ _ hagents, r _ _ hguard, r _ _ htools]

theorem runStep_identity_immutable (validate guard : Doc → Bool) (t : Target)
    (s : Store) (p : Patch) (k : FieldKey) (hk : k ∈ protectedFields t) :
    (runStep validate guard t s p).1 t k = s t k := by
  cases hstep : step validate guard t (s t) p with
  | none => simp [runStep, hstep]
  | some merged =>
      have hm := step_accepts_wholesale validate guard t (s t) p merged hstep
      have himm := identity_immutable t (s t) p k hk
      simp [runStep, hstep, hm, himm]

theorem Grants.le_refl (g : Grants) : g.le g = true := by
  cases h : g.packInstall <;> simp [Grants.le, h]

theorem Grants.boundedBy_self (g held : Grants) : g.boundedBy g held = true := by
  cases h : g.packInstall <;> simp [Grants.boundedBy, h]

/-- A write whose grants stay within the stored Tools is accepted whatever
the invoker holds. -/
theorem Grants.boundedBy_of_le_stored {c s : Grants} (held : Grants)
    (h : c.le s = true) : c.boundedBy s held = true := by
  cases hc : c.packInstall <;> cases hs : s.packInstall <;>
    simp_all [Grants.le, Grants.boundedBy]

/-- A write whose pack installation stays within what the invoker holds is
accepted whatever the stored Tools carry: a holder may grant pack installation
to a sibling. -/
theorem Grants.boundedBy_of_le_held {c held : Grants} (s : Grants)
    (h : c.le held = true) : c.boundedBy s held = true := by
  cases hc : c.packInstall <;> cases hh : held.packInstall <;>
    simp_all [Grants.le, Grants.boundedBy]

/-- Raising pack installation above the stored Tools needs the invoker to
hold it, whatever other grants the documents carry. -/
theorem grant_widening_requires_held (decode : Doc → Option Grants) (held : Grants)
    (stored candidate : Doc) (s c : Grants)
    (hs : decode stored = some s) (hc : decode candidate = some c)
    (hcp : c.packInstall = true) (hsp : s.packInstall = false)
    (h : keepsGrants decode held stored candidate = true) :
    held.packInstall = true := by
  simpa [keepsGrants, Grants.boundedBy, hs, hc, hcp, hsp] using h

/-- An edit that leaves the grants as stored is accepted whatever the invoker
holds. -/
theorem unrelated_edit_preserves_grants (decode : Doc → Option Grants) (held : Grants)
    (stored candidate : Doc) (g : Grants)
    (hs : decode stored = some g) (hc : decode candidate = some g) :
    keepsGrants decode held stored candidate = true := by
  simp [keepsGrants, hs, hc, Grants.boundedBy_self]

/-- Holding no grant, raising pack installation is refused. -/
theorem self_grant_without_held_refused (decode : Doc → Option Grants)
    (stored candidate : Doc)
    (hs : decode stored = some Grants.bot)
    (hc : decode candidate = some { Grants.bot with packInstall := true }) :
    keepsGrants decode Grants.bot stored candidate = false := by
  simp [keepsGrants, hs, hc, Grants.boundedBy, Grants.bot]

/-- An accepted Tools write under the always-on grant guard keeps the bound,
whatever the opt-in lockout guard decides. -/
theorem accepted_tools_write_keeps_grants (decode : Doc → Option Grants) (held : Grants)
    (validate lockout : Doc → Bool) (stored : Doc) (p : Patch) (merged : Doc)
    (h : step validate
      (fun candidate => keepsGrants decode held stored candidate && lockout candidate)
      .tools stored p = some merged) :
    keepsGrants decode held stored merged = true := by
  have hg := (step_accept_validates validate _ .tools stored p merged h).2
  simp only [Bool.and_eq_true] at hg
  exact hg.1

end SelfConfig
