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

theorem self_config_disable_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (hoff : new.selfConfig = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, ho, hn, hoff]

theorem agents_removal_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.agents = true) (removed : new.agents = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, retained, ho, hn, had, removed]

theorem no_lockout_removal_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.noLockout = true) (removed : new.noLockout = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, retained, ho, hn, had, removed]

theorem tools_authority_removal_refused (decode : Doc → Option Control)
    (stored candidate : Doc) (old new : Control)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.toolsAuthority = true) (removed : new.toolsAuthority = false) :
    keepsControl decode stored candidate = false := by
  simp [keepsControl, retained, ho, hn, had, removed]

theorem self_disable_refused (decode : Doc → Option Reach)
    (stored candidate : Doc) (old new : Reach)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (hoff : new.enabled = false) :
    keepsReach decode stored candidate = false := by
  simp [keepsReach, ho, hn, hoff]

theorem engineer_tag_removal_refused (decode : Doc → Option Reach)
    (stored candidate : Doc) (old new : Reach)
    (ho : decode stored = some old) (hn : decode candidate = some new)
    (had : old.engineerTag = true) (removed : new.engineerTag = false) :
    keepsReach decode stored candidate = false := by
  simp [keepsReach, retained, ho, hn, had, removed]

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

theorem Grants.boundedBy_of_le_stored {c s : Grants} (held : Grants)
    (h : c.le s = true) : c.boundedBy s held = true := by
  cases hc : c.packInstall <;> cases hs : s.packInstall <;>
    simp_all [Grants.le, Grants.boundedBy]

theorem Grants.boundedBy_of_le_held {c held : Grants} (s : Grants)
    (h : c.le held = true) : c.boundedBy s held = true := by
  cases hc : c.packInstall <;> cases hh : held.packInstall <;>
    simp_all [Grants.le, Grants.boundedBy]

theorem ceiling_leB_refl (c : PluginJob.Ceiling) : c.leB c = true := by
  cases h : c.targets <;> simp [PluginJob.Ceiling.leB, PluginJob.scopeLe, h]

theorem JobGrant.narrows_refl (g : Option JobGrant) : JobGrant.narrows g g = true := by
  cases g with
  | none => rfl
  | some j =>
      have h : j.ceiling ≤ j.ceiling := ceiling_leB_refl j.ceiling
      simp [JobGrant.narrows, h]

/-- The coordinates of the job blocks a Tools document carries are distinct. -/
def Grants.jobCoordinatesUnique (g : Grants) : Prop :=
  g.job.Pairwise fun a b => a.1 ≠ b.1

private theorem lookup_eq_some_of_mem {l : List (String × JobGrant)}
    (hu : l.Pairwise fun a b => a.1 ≠ b.1) {k : String} {v : JobGrant}
    (hm : (k, v) ∈ l) : l.lookup k = some v := by
  induction hu with
  | nil => simp at hm
  | @cons e t hne _ ih =>
      rcases List.mem_cons.mp hm with rfl | hm
      · simp [List.lookup]
      · have hk : k ≠ e.1 := fun hke => hne _ hm (hke ▸ rfl)
        have hb : (k == e.1) = false := by simpa using hk
        simp [List.lookup, hb, ih hm]

theorem keepsJobGrants_self (g : Grants) (hu : g.jobCoordinatesUnique) :
    keepsJobGrants g g = true := by
  simp only [keepsJobGrants, List.all_eq_true]
  intro e he
  rw [lookup_eq_some_of_mem hu he]
  exact JobGrant.narrows_refl _

theorem grant_widening_requires_held (decode : Doc → Option Grants) (held : Grants)
    (stored candidate : Doc) (s c : Grants)
    (hs : decode stored = some s) (hc : decode candidate = some c)
    (hcp : c.packInstall = true) (hsp : s.packInstall = false)
    (h : keepsGrants decode held stored candidate = true) :
    held.packInstall = true := by
  simp [keepsGrants, Grants.boundedBy, hs, hc, hcp, hsp] at h
  exact h.1

theorem unrelated_edit_preserves_grants (decode : Doc → Option Grants) (held : Grants)
    (stored candidate : Doc) (g : Grants)
    (hs : decode stored = some g) (hc : decode candidate = some g)
    (hu : g.jobCoordinatesUnique) :
    keepsGrants decode held stored candidate = true := by
  simp [keepsGrants, hs, hc, Grants.boundedBy_self, keepsJobGrants_self g hu]

theorem self_grant_without_held_refused (decode : Doc → Option Grants)
    (stored candidate : Doc)
    (hs : decode stored = some Grants.bot)
    (hc : decode candidate = some { Grants.bot with packInstall := true }) :
    keepsGrants decode Grants.bot stored candidate = false := by
  simp [keepsGrants, hs, hc, Grants.boundedBy, Grants.bot]

theorem accepted_tools_write_keeps_grants (decode : Doc → Option Grants) (held : Grants)
    (validate lockout : Doc → Bool) (stored : Doc) (p : Patch) (merged : Doc)
    (h : step validate
      (fun candidate => keepsGrants decode held stored candidate && lockout candidate)
      .tools stored p = some merged) :
    keepsGrants decode held stored merged = true := by
  have hg := (step_accept_validates validate _ .tools stored p merged h).2
  simp only [Bool.and_eq_true] at hg
  exact hg.1

theorem chain_widening_requires_held (decode : Doc → Option Grants) (held : Grants)
    (resolve : Doc → Option Doc) (stored candidate storedTools candidateTools : Doc)
    (s c : Grants)
    (hs : resolve stored = some storedTools) (hc : resolve candidate = some candidateTools)
    (hds : decode storedTools = some s) (hdc : decode candidateTools = some c)
    (hcp : c.packInstall = true) (hsp : s.packInstall = false)
    (h : chainKeepsGrants decode held resolve stored candidate = true) :
    held.packInstall = true := by
  simp only [chainKeepsGrants, reselectionKeepsGrants, hs, hc] at h
  exact grant_widening_requires_held decode held storedTools candidateTools s c hds hdc hcp hsp h

theorem chain_unchanged_selection_keeps_grants (decode : Doc → Option Grants)
    (held : Grants) (resolve : Doc → Option Doc) (stored candidate tools : Doc) (g : Grants)
    (hs : resolve stored = some tools) (hc : resolve candidate = some tools)
    (hd : decode tools = some g) (hu : g.jobCoordinatesUnique) :
    chainKeepsGrants decode held resolve stored candidate = true := by
  simp only [chainKeepsGrants, reselectionKeepsGrants, hs, hc]
  exact unrelated_edit_preserves_grants decode held tools tools g hd hd hu

theorem chain_without_tools_is_unconstrained (decode : Doc → Option Grants)
    (held : Grants) (resolve : Doc → Option Doc) (stored candidate : Doc)
    (hc : resolve candidate = none) :
    chainKeepsGrants decode held resolve stored candidate = true := by
  cases hs : resolve stored <;> simp [chainKeepsGrants, reselectionKeepsGrants, hs, hc]

theorem reselection_from_nothing_requires_held (decode : Doc → Option Grants)
    (held : Grants) (tools : Doc) (c : Grants)
    (hd : decode tools = some c) (hcp : c.packInstall = true)
    (h : reselectionKeepsGrants decode held none (some tools) = true) :
    held.packInstall = true := by
  simp [reselectionKeepsGrants, hd, Grants.boundedBy, Grants.bot, hcp] at h
  exact h.1

private theorem mem_of_lookup_eq_some {l : List (String × JobGrant)} {k : String}
    {v : JobGrant} (h : l.lookup k = some v) : (k, v) ∈ l := by
  induction l with
  | nil => simp [List.lookup] at h
  | cons e t ih =>
      obtain ⟨a, b⟩ := e
      cases hb : (k == a)
      · simp only [List.lookup, hb] at h
        exact List.mem_cons_of_mem _ (ih h)
      · have hka : k = a := by simpa using hb
        subst hka
        simp only [List.lookup, hb, Option.some.injEq] at h
        subst h
        simp

theorem keepsJobGrants_lookup {c s : Grants} {plugin : String} {jc : JobGrant}
    (h : keepsJobGrants c s = true) (hj : c.job.lookup plugin = some jc) :
    JobGrant.narrows (some jc) (s.job.lookup plugin) = true := by
  have hm := mem_of_lookup_eq_some hj
  exact List.all_eq_true.mp h _ hm

/-- A later job entry for a plugin already listed is judged on its own: it cannot
hide behind the first entry for that coordinate. -/
theorem keepsJobGrants_checks_every_entry (plugin : String) (a b : JobGrant)
    (hb : JobGrant.narrows (some b) (some a) = false) :
    keepsJobGrants { packInstall := false, job := [(plugin, a), (plugin, b)] }
      { packInstall := false, job := [(plugin, a)] } = false := by
  simp [keepsJobGrants, List.lookup, hb]

/-- A model Tools write keeps every job ceiling within the stored block of the
same plugin, whatever the invoker holds. -/
theorem job_ceiling_self_config_narrow_only (decode : Doc → Option Grants) (held : Grants)
    (validate lockout : Doc → Bool) (stored : Doc) (p : Patch) (merged : Doc)
    (s c : Grants) (plugin : String) (jc : JobGrant)
    (h : step validate
      (fun candidate => keepsGrants decode held stored candidate && lockout candidate)
      .tools stored p = some merged)
    (hs : decode stored = some s) (hc : decode merged = some c)
    (hj : c.job.lookup plugin = some jc) :
    ∃ js, s.job.lookup plugin = some js ∧ jc.ceiling ≤ js.ceiling := by
  have hk := accepted_tools_write_keeps_grants decode held validate lockout stored p merged h
  simp only [keepsGrants, hs, hc, Bool.and_eq_true] at hk
  have hn := keepsJobGrants_lookup hk.2 hj
  cases hsl : s.job.lookup plugin with
  | none => simp [hsl, JobGrant.narrows] at hn
  | some js =>
      refine ⟨js, rfl, ?_⟩
      simp [hsl, JobGrant.narrows] at hn
      exact hn.1

/-- A model Tools write keeps each job block's policy as stored. -/
theorem job_policy_operator_only (decode : Doc → Option Grants) (held : Grants)
    (validate lockout : Doc → Bool) (stored : Doc) (p : Patch) (merged : Doc)
    (s c : Grants) (plugin : String) (jc : JobGrant)
    (h : step validate
      (fun candidate => keepsGrants decode held stored candidate && lockout candidate)
      .tools stored p = some merged)
    (hs : decode stored = some s) (hc : decode merged = some c)
    (hj : c.job.lookup plugin = some jc) :
    ∃ js, s.job.lookup plugin = some js ∧ jc.policy = js.policy := by
  have hk := accepted_tools_write_keeps_grants decode held validate lockout stored p merged h
  simp only [keepsGrants, hs, hc, Bool.and_eq_true] at hk
  have hn := keepsJobGrants_lookup hk.2 hj
  cases hsl : s.job.lookup plugin with
  | none => simp [hsl, JobGrant.narrows] at hn
  | some js =>
      refine ⟨js, rfl, ?_⟩
      simp [hsl, JobGrant.narrows] at hn
      exact hn.2

/-- A model Tools write cannot add a job block for a plugin the stored Tools
carry none for. -/
theorem self_config_cannot_add_job_block (decode : Doc → Option Grants) (held : Grants)
    (validate lockout : Doc → Bool) (stored : Doc) (p : Patch) (merged : Doc)
    (s c : Grants) (plugin : String)
    (h : step validate
      (fun candidate => keepsGrants decode held stored candidate && lockout candidate)
      .tools stored p = some merged)
    (hs : decode stored = some s) (hc : decode merged = some c)
    (hnone : s.job.lookup plugin = none) :
    c.job.lookup plugin = none := by
  have hk := accepted_tools_write_keeps_grants decode held validate lockout stored p merged h
  simp only [keepsGrants, hs, hc, Bool.and_eq_true] at hk
  cases hcl : c.job.lookup plugin with
  | none => rfl
  | some jc =>
      have hn := keepsJobGrants_lookup hk.2 hcl
      simp [hnone, JobGrant.narrows] at hn

/-- A Context write that newly selects Tools, with no previous selection,
acquires no job block, whatever the invoker holds. -/
theorem reselection_cannot_acquire_job_ceiling (decode : Doc → Option Grants)
    (held : Grants) (validate lockout : Doc → Bool) (resolve : Doc → Option Doc)
    (stored : Doc) (p : Patch) (merged tools : Doc) (c : Grants) (plugin : String)
    (h : step validate
      (fun candidate => chainKeepsGrants decode held resolve stored candidate && lockout candidate)
      .agentContext stored p = some merged)
    (hs : resolve stored = none) (hm : resolve merged = some tools)
    (hd : decode tools = some c) :
    c.job.lookup plugin = none := by
  have hg := (step_accept_validates validate _ .agentContext stored p merged h).2
  simp only [Bool.and_eq_true] at hg
  have hk := hg.1
  simp only [chainKeepsGrants, reselectionKeepsGrants, hs, hm, hd, Option.any_some,
    Bool.and_eq_true] at hk
  cases hcl : c.job.lookup plugin with
  | none => rfl
  | some jc =>
      have hn := keepsJobGrants_lookup hk.2 hcl
      simp [Grants.bot, JobGrant.narrows, List.lookup] at hn

end SelfConfig
