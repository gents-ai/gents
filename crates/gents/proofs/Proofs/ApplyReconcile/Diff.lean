import Proofs.ApplyReconcile.Manifest

namespace ApplyReconcile

inductive ApplyStep where
  | create (d : DocRef) (f : DesiredFields)
  | update (d : DocRef) (f : DesiredFields)
  | delete (d : DocRef)

namespace ApplyStep

def target : ApplyStep → DocRef
  | .create d _ => d
  | .update d _ => d
  | .delete d => d

def payload? : ApplyStep → Option DesiredFields
  | .create _ f => some f
  | .update _ f => some f
  | .delete _ => none

def payload : ApplyStep → DesiredFields
  | .create _ f => f
  | .update _ f => f
  | .delete _ => { content := "", refs := ∅ }

def isDelete : ApplyStep → Bool
  | .delete _ => true
  | _ => false

def isWrite (s : ApplyStep) : Bool := s.payload?.isSome

def le (a b : ApplyStep) : Bool := DocRef.le a.target b.target

def deleteLe (a b : ApplyStep) : Bool := DocRef.le b.target a.target

end ApplyStep

noncomputable def diff (M : Manifest) (L : LiveState) : List ApplyStep :=
  (M.support.toList.filterMap (fun d =>
    match M.docs d, L.desired d with
    | some f, none     => some (ApplyStep.create d f)
    | some f, some g   => if f = g then none else some (ApplyStep.update d f)
    | none,   _        => none)).mergeSort ApplyStep.le

abbrev LiveSupport := Finset DocRef

def LiveSupportComplete (L : LiveState) (support : LiveSupport) : Prop :=
  ∀ d : DocRef, L.contains d = true → d ∈ support

def liveReferences (L : LiveState) (referrer target : DocRef) : Prop :=
  ∃ f, L.desired referrer = some f ∧ target ∈ referencesOf f

def liveReferencesBool (L : LiveState) (referrer target : DocRef) : Bool :=
  match L.desired referrer with
  | some f => target ∈ referencesOf f
  | none => false

def deleteSafe (L : LiveState) (target : DocRef) : Prop :=
  ∀ referrer : DocRef, ¬ liveReferences L referrer target

noncomputable def noLiveReferencesIn (L : LiveState) (support : LiveSupport) (target : DocRef) : Bool :=
  support.toList.all (fun referrer => !(liveReferencesBool L referrer target))

lemma liveReferencesBool_eq_true
    {L : LiveState} {referrer target : DocRef} :
    liveReferencesBool L referrer target = true ↔ liveReferences L referrer target := by
  unfold liveReferencesBool liveReferences
  cases h : L.desired referrer with
  | none =>
      simp [h]
  | some f =>
      simp [h]

lemma noLiveReferencesIn_deleteSafe
    {L : LiveState} {support : LiveSupport} {target : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hcheck : noLiveReferencesIn L support target = true) :
    deleteSafe L target := by
  intro referrer href
  unfold noLiveReferencesIn at hcheck
  rcases href with ⟨f, hdesired, hrefTarget⟩
  have hcontains : L.contains referrer = true := by
    simp [LiveState.contains, hdesired]
  have hsupport : referrer ∈ support := hcomplete referrer hcontains
  have hlist : referrer ∈ support.toList := by
    exact (Finset.mem_toList).2 hsupport
  have hnot := (List.all_eq_true.mp hcheck) referrer hlist
  have hfalse : liveReferencesBool L referrer target = false := by
    simpa [Bool.not_eq_true] using hnot
  have htrue : liveReferencesBool L referrer target = true :=
    liveReferencesBool_eq_true.mpr ⟨f, hdesired, hrefTarget⟩
  rw [htrue] at hfalse
  cases hfalse

noncomputable def managedDeletes
    (M : Manifest) (L : LiveState) (support : LiveSupport) : List ApplyStep :=
  (support.toList.filterMap (fun d =>
    if d.collection.manifestAuthoritative then
      if M.contains d then none
      else if L.contains d then
        if noLiveReferencesIn L support d then some (ApplyStep.delete d) else none
      else none
    else none)).mergeSort ApplyStep.deleteLe

noncomputable def pruneDeletes
    (M : Manifest) (L : LiveState) (support : LiveSupport) : List ApplyStep :=
  (support.toList.filterMap (fun d =>
    if d.collection.manifestAuthoritative then none
    else if M.contains d then none
    else if L.contains d then
      if noLiveReferencesIn L support d then some (ApplyStep.delete d) else none
    else none)).mergeSort ApplyStep.deleteLe

noncomputable def diffManaged
    (M : Manifest) (L : LiveState) (support : LiveSupport) : List ApplyStep :=
  diff M L ++ managedDeletes M L support

noncomputable def diffPrune
    (M : Manifest) (L : LiveState) (support : LiveSupport) : List ApplyStep :=
  diffManaged M L support ++ pruneDeletes M L support

private lemma DocRef.le_elim {a b : DocRef} (h : DocRef.le a b = true) :
    a.collection.applyOrder < b.collection.applyOrder ∨
    (a.collection.applyOrder = b.collection.applyOrder ∧ a.id ≤ b.id) := by
  unfold DocRef.le at h
  by_cases h1 : a.collection.applyOrder < b.collection.applyOrder
  · exact Or.inl h1
  · by_cases h2 : a.collection.applyOrder > b.collection.applyOrder
    · simp [h1, h2] at h
    · have heq : a.collection.applyOrder = b.collection.applyOrder := by omega
      simp [h1, h2] at h
      exact Or.inr ⟨heq, h⟩

private lemma DocRef.le_intro {a b : DocRef}
    (h : a.collection.applyOrder < b.collection.applyOrder ∨
         (a.collection.applyOrder = b.collection.applyOrder ∧ a.id ≤ b.id)) :
    DocRef.le a b = true := by
  unfold DocRef.le
  rcases h with hlt | ⟨heq, hid⟩
  · simp [hlt]
  · have h1 : ¬ a.collection.applyOrder < b.collection.applyOrder := by omega
    have h2 : ¬ a.collection.applyOrder > b.collection.applyOrder := by omega
    simp [h1, h2, hid]

lemma ApplyStep.le_trans (a b c : ApplyStep)
    (hab : ApplyStep.le a b = true) (hbc : ApplyStep.le b c = true) :
    ApplyStep.le a c = true := by
  unfold ApplyStep.le at hab hbc ⊢
  rcases DocRef.le_elim hab with hab_rank | ⟨hab_eq, hab_id⟩
  · rcases DocRef.le_elim hbc with hbc_rank | ⟨hbc_eq, _⟩
    · exact DocRef.le_intro (Or.inl (Nat.lt_trans hab_rank hbc_rank))
    · exact DocRef.le_intro (Or.inl (hbc_eq ▸ hab_rank))
  · rcases DocRef.le_elim hbc with hbc_rank | ⟨hbc_eq, hbc_id⟩
    · exact DocRef.le_intro (Or.inl (hab_eq ▸ hbc_rank))
    · refine DocRef.le_intro (Or.inr ⟨hab_eq.trans hbc_eq, ?_⟩)
      exact String.le_trans hab_id hbc_id

lemma ApplyStep.le_total (a b : ApplyStep) :
    ApplyStep.le a b || ApplyStep.le b a := by
  unfold ApplyStep.le
  by_cases h1 : a.target.collection.applyOrder < b.target.collection.applyOrder
  · have := DocRef.le_intro (a := a.target) (b := b.target) (Or.inl h1)
    simp [this]
  · by_cases h2 : b.target.collection.applyOrder < a.target.collection.applyOrder
    · have := DocRef.le_intro (a := b.target) (b := a.target) (Or.inl h2)
      simp [this]
    · have heq : a.target.collection.applyOrder = b.target.collection.applyOrder := by omega
      rcases String.le_total a.target.id b.target.id with hid | hid
      · have := DocRef.le_intro (a := a.target) (b := b.target) (Or.inr ⟨heq, hid⟩)
        simp [this]
      · have := DocRef.le_intro (a := b.target) (b := a.target) (Or.inr ⟨heq.symm, hid⟩)
        simp [this]

lemma diff_pairwise_le (M : Manifest) (L : LiveState) :
    (diff M L).Pairwise (fun a b => ApplyStep.le a b = true) := by
  unfold diff
  have hsort := List.sorted_mergeSort
    (le := ApplyStep.le)
    (trans := fun a b c => ApplyStep.le_trans a b c)
    (total := fun a b => ApplyStep.le_total a b)
    ((M.support.toList.filterMap (fun d =>
      match M.docs d, L.desired d with
      | some f, none     => some (ApplyStep.create d f)
      | some f, some g   => if f = g then none else some (ApplyStep.update d f)
      | none,   _        => none)))
  exact hsort

lemma diff_step_payload_eq_some
    {M : Manifest} {L : LiveState} {s : ApplyStep}
    (hmem : s ∈ diff M L) :
    s.payload? = some s.payload := by
  unfold diff at hmem
  rw [List.mem_mergeSort, List.mem_filterMap] at hmem
  obtain ⟨d, _hd, hprod⟩ := hmem
  revert hprod
  cases hMd : M.docs d with
  | none =>
      cases hLd : L.desired d with
      | none => intro h; simp at h
      | some _ => intro h; simp at h
  | some f =>
      cases hLd : L.desired d with
      | none =>
          intro h
          simp at h
          rw [← h]
          rfl
      | some g =>
          intro h
          by_cases hfg : f = g
          · subst hfg
            simp at h
          · simp [hfg] at h
            rw [← h]
            rfl

lemma diff_step_payload_isSome
    {M : Manifest} {L : LiveState} {s : ApplyStep}
    (hmem : s ∈ diff M L) :
    s.payload?.isSome = true := by
  rw [diff_step_payload_eq_some hmem]
  rfl

theorem delete_order_referrers_before_dependencies
    {referrer dependency : DocRef}
    (hrank : dependency.collection.applyOrder < referrer.collection.applyOrder) :
    ApplyStep.deleteLe (ApplyStep.delete referrer) (ApplyStep.delete dependency) = true := by
  unfold ApplyStep.deleteLe ApplyStep.target
  exact DocRef.le_intro (Or.inl hrank)

theorem managedDeletes_emits_only_safe_deletes
    {M : Manifest} {L : LiveState} {support : LiveSupport} {s : ApplyStep}
    (hcomplete : LiveSupportComplete L support)
    (hmem : s ∈ managedDeletes M L support) :
    ∃ d, s = ApplyStep.delete d ∧ deleteSafe L d := by
  unfold managedDeletes at hmem
  rw [List.mem_mergeSort, List.mem_filterMap] at hmem
  rcases hmem with ⟨d, _hd, hprod⟩
  by_cases hOwned : d.collection.manifestAuthoritative = true
  · by_cases hM : M.contains d = true
    · simp [hOwned, hM] at hprod
    · have hMfalse : M.contains d = false := by
        cases h : M.contains d <;> simp [h] at hM ⊢
      by_cases hL : L.contains d = true
      · by_cases hcheck : noLiveReferencesIn L support d = true
        · simp [hOwned, hMfalse, hL, hcheck] at hprod
          subst hprod
          exact ⟨d, rfl, noLiveReferencesIn_deleteSafe hcomplete hcheck⟩
        · have hcheckFalse : noLiveReferencesIn L support d = false := by
            cases h : noLiveReferencesIn L support d <;> simp [h] at hcheck ⊢
          simp [hOwned, hMfalse, hL, hcheckFalse] at hprod
      · have hLfalse : L.contains d = false := by
          cases h : L.contains d <;> simp [h] at hL ⊢
        simp [hOwned, hMfalse, hLfalse] at hprod
  · have hOwnedFalse : d.collection.manifestAuthoritative = false := by
      cases h : d.collection.manifestAuthoritative <;> simp [h] at hOwned ⊢
    simp [hOwnedFalse] at hprod

theorem pruneDeletes_emits_only_safe_deletes
    {M : Manifest} {L : LiveState} {support : LiveSupport} {s : ApplyStep}
    (hcomplete : LiveSupportComplete L support)
    (hmem : s ∈ pruneDeletes M L support) :
    ∃ d, s = ApplyStep.delete d ∧ deleteSafe L d := by
  unfold pruneDeletes at hmem
  rw [List.mem_mergeSort, List.mem_filterMap] at hmem
  rcases hmem with ⟨d, _hd, hprod⟩
  by_cases hOwned : d.collection.manifestAuthoritative = true
  · simp [hOwned] at hprod
  · have hOwnedFalse : d.collection.manifestAuthoritative = false := by
      cases h : d.collection.manifestAuthoritative <;> simp [h] at hOwned ⊢
    by_cases hM : M.contains d = true
    · simp [hOwnedFalse, hM] at hprod
    · have hMfalse : M.contains d = false := by
        cases h : M.contains d <;> simp [h] at hM ⊢
      by_cases hL : L.contains d = true
      · by_cases hcheck : noLiveReferencesIn L support d = true
        · simp [hOwnedFalse, hMfalse, hL, hcheck] at hprod
          subst hprod
          exact ⟨d, rfl, noLiveReferencesIn_deleteSafe hcomplete hcheck⟩
        · have hcheckFalse : noLiveReferencesIn L support d = false := by
            cases h : noLiveReferencesIn L support d <;> simp [h] at hcheck ⊢
          simp [hOwnedFalse, hMfalse, hL, hcheckFalse] at hprod
      · have hLfalse : L.contains d = false := by
          cases h : L.contains d <;> simp [h] at hL ⊢
        simp [hOwnedFalse, hMfalse, hLfalse] at hprod

theorem pruneDeletes_deleteSafe
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ pruneDeletes M L support) :
    deleteSafe L d := by
  rcases pruneDeletes_emits_only_safe_deletes hcomplete hmem with ⟨d', hstep, hsafe⟩
  cases hstep
  exact hsafe

theorem managedDeletes_deleteSafe
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ managedDeletes M L support) :
    deleteSafe L d := by
  rcases managedDeletes_emits_only_safe_deletes hcomplete hmem with ⟨d', hstep, hsafe⟩
  cases hstep
  exact hsafe

theorem diffManaged_deleteSafe
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ diffManaged M L support) :
    deleteSafe L d := by
  unfold diffManaged at hmem
  rw [List.mem_append] at hmem
  rcases hmem with hdiff | hmanaged
  · have hpayload := diff_step_payload_isSome hdiff
    simp [ApplyStep.payload?] at hpayload
  · exact managedDeletes_deleteSafe hcomplete hmanaged

theorem diffPrune_deleteSafe
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ diffPrune M L support) :
    deleteSafe L d := by
  unfold diffPrune at hmem
  rw [List.mem_append] at hmem
  rcases hmem with hmanaged | hprune
  · exact diffManaged_deleteSafe hcomplete hmanaged
  · exact pruneDeletes_deleteSafe hcomplete hprune

theorem t_managed_delete_safety
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ diffManaged M L support) :
    ∀ referrer : DocRef, ¬ liveReferences L referrer d :=
  diffManaged_deleteSafe hcomplete hmem

theorem t_delete_safety
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ diffPrune M L support) :
    ∀ referrer : DocRef, ¬ liveReferences L referrer d :=
  diffPrune_deleteSafe hcomplete hmem

lemma diff_sorted_by_applyOrder (M : Manifest) (L : LiveState)
    (i j : Nat) (hij : i ≤ j) (hj : j < (diff M L).length) :
    ((diff M L).get ⟨i, Nat.lt_of_le_of_lt hij hj⟩).target.collection.applyOrder
      ≤ ((diff M L).get ⟨j, hj⟩).target.collection.applyOrder := by
  rcases Nat.lt_or_eq_of_le hij with hlt | heq
  ·
    have hpw := diff_pairwise_le M L
    have hi : i < (diff M L).length := Nat.lt_of_le_of_lt hij hj
    have hle := (List.pairwise_iff_getElem.mp hpw) i j hi hj hlt
    unfold ApplyStep.le at hle
    simp only [List.get_eq_getElem]
    rcases DocRef.le_elim hle with hrank | ⟨heq, _⟩
    · exact Nat.le_of_lt hrank
    · exact Nat.le_of_eq heq
  ·
    subst heq
    exact Nat.le_refl _

end ApplyReconcile
