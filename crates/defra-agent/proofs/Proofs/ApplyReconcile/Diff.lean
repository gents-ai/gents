import Proofs.ApplyReconcile.Manifest

/-!
# Apply/Reconcile Diff

Apply-step vocabulary, diff construction, and ordering lemmas.
-/

namespace ApplyReconcile

/-- A single mutation landing in the DB from the apply agent.
    Create/update carry only `DesiredFields` -- no `LiveFields`
    constructor exists, which is the Lean-side restatement of the
    Rust `DesiredFields` bound on the apply boundary. Delete targets a
    document by identity only; it has no desired payload. -/
inductive ApplyStep where
  | create (d : DocRef) (f : DesiredFields)
  | update (d : DocRef) (f : DesiredFields)
  | delete (d : DocRef)

namespace ApplyStep

def target : ApplyStep → DocRef
  | .create d _ => d
  | .update d _ => d
  | .delete d => d

/-- Optional desired payload. Delete intentionally returns `none`; this keeps
    delete outside the payload-bearing write path. -/
def payload? : ApplyStep → Option DesiredFields
  | .create _ f => some f
  | .update _ f => some f
  | .delete _ => none

/-- Payload projection for write-step proofs over non-prune diffs. For delete
    this returns an inert empty payload; semantic apply/delete behavior must use
    `payload?`, not this helper. -/
def payload : ApplyStep → DesiredFields
  | .create _ f => f
  | .update _ f => f
  | .delete _ => { content := "", refs := ∅ }

def isDelete : ApplyStep → Bool
  | .delete _ => true
  | _ => false

def isWrite (s : ApplyStep) : Bool := s.payload?.isSome

/-- Step-level comparator: orders by target's DocRef ordering.
    `Bool`-valued so it can drive `List.mergeSort`. -/
def le (a b : ApplyStep) : Bool := DocRef.le a.target b.target

/-- Delete comparator: reverse target order, so referrers (higher apply ranks)
    are visited before dependencies (lower apply ranks). -/
def deleteLe (a b : ApplyStep) : Bool := DocRef.le b.target a.target

end ApplyStep


/-- Diff M against L, producing a list of apply-steps enumerated over
    `M.support`. `live_only` documents (present in L but not in M) do
    not produce steps — they are reporting-only, consistent with the
    spec's non-goals on delete.

    The `none, _` arm of the `match` handles an impossible case: if
    `d ∈ M.support` then `M.docs d` is `some` by `support_iff`. We
    return `none` there because `filterMap` simply skips it; the arm
    is dead code from the well-formed entry path.

    The output is sorted by `(collection.applyOrder, id)` via
    `List.mergeSort` with `ApplyStep.le`, so rank-0 targets (potential
    reference targets like backends and selections) precede rank-2
    referrers (e.g. agent behaviors). Downstream proofs of reference
    closure rely on this ordering. -/
noncomputable def diff (M : Manifest) (L : LiveState) : List ApplyStep :=
  (M.support.toList.filterMap (fun d =>
    match M.docs d, L.desired d with
    | some f, none     => some (ApplyStep.create d f)
    | some f, some g   => if f = g then none else some (ApplyStep.update d f)
    | none,   _        => none)).mergeSort ApplyStep.le

/-- Finite support witness for live desired rows. `LiveState.desired` is a
    partial function, so prune mode takes the finite support supplied by the
    concrete diff implementation. -/
abbrev LiveSupport := Finset DocRef

/-- The caller-supplied live support is complete when it includes every present
    desired row. This is the bridge from the executable finite check used by
    prune generation to the globally-quantified `deleteSafe` property. -/
def LiveSupportComplete (L : LiveState) (support : LiveSupport) : Prop :=
  ∀ d : DocRef, L.contains d = true → d ∈ support

/-- A live desired row `referrer` declares a structural reference to `target`. -/
def liveReferences (L : LiveState) (referrer target : DocRef) : Prop :=
  ∃ f, L.desired referrer = some f ∧ target ∈ referencesOf f

/-- Boolean counterpart used by executable prune witnesses. -/
def liveReferencesBool (L : LiveState) (referrer target : DocRef) : Bool :=
  match L.desired referrer with
  | some f => target ∈ referencesOf f
  | none => false

/-- Delete is permitted at a state only when no live desired row references the
    target. `AgentRequest.caused_by_trigger_id`-style lineage strings are not
    modeled here: only structural desired-state references in `DesiredFields.refs`
    participate in this relation. -/
def deleteSafe (L : LiveState) (target : DocRef) : Prop :=
  ∀ referrer : DocRef, ¬ liveReferences L referrer target

/-- Finite executable check for the concrete live support supplied to prune
    diff generation. -/
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

/-- A successful finite prune-support check implies the global delete-safety
    predicate when the support covers every present desired row. -/
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

/-- Prune-mode delete steps for live-only desired rows. The default `diff`
    above remains byte-for-byte create/update only; callers opt into these
    deletes explicitly and must supply the finite live desired support. -/
noncomputable def pruneDeletes
    (M : Manifest) (L : LiveState) (support : LiveSupport) : List ApplyStep :=
  (support.toList.filterMap (fun d =>
    if M.contains d then none
    else if L.contains d then
      if noLiveReferencesIn L support d then some (ApplyStep.delete d) else none
    else none)).mergeSort ApplyStep.deleteLe

/-- Prune-mode apply diff: write desired create/update steps first, then the
    opt-in delete sequence over live-only rows. -/
noncomputable def diffPrune
    (M : Manifest) (L : LiveState) (support : LiveSupport) : List ApplyStep :=
  diff M L ++ pruneDeletes M L support

/-- Extract rank and id-level obligations from a true `DocRef.le`. -/
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

/-- Build a true `DocRef.le` from the rank/id-level disjunction. -/
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

/-- Transitivity of the step-level comparator, via the lexicographic
    `DocRef.le`. -/
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

/-- Totality (Bool form) of the step-level comparator. -/
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

/-- `diff M L` is pairwise-sorted by `ApplyStep.le`. -/
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

/-- The default (non-prune) diff emits only create/update steps. -/
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

/-- The default (non-prune) diff emits only create/update steps. -/
lemma diff_step_payload_isSome
    {M : Manifest} {L : LiveState} {s : ApplyStep}
    (hmem : s ∈ diff M L) :
    s.payload?.isSome = true := by
  rw [diff_step_payload_eq_some hmem]
  rfl

/-- A reverse delete order visits a referrer before any lower-rank dependency it
    references. This is the Lean shape of the production prune-order safety
    argument. -/
theorem delete_order_referrers_before_dependencies
    {referrer dependency : DocRef}
    (hrank : dependency.collection.applyOrder < referrer.collection.applyOrder) :
    ApplyStep.deleteLe (ApplyStep.delete referrer) (ApplyStep.delete dependency) = true := by
  unfold ApplyStep.deleteLe ApplyStep.target
  exact DocRef.le_intro (Or.inl hrank)

/-- Every emitted prune-delete is globally delete-safe, provided the finite
    support supplied to prune generation covers every live desired row. -/
theorem pruneDeletes_emits_only_safe_deletes
    {M : Manifest} {L : LiveState} {support : LiveSupport} {s : ApplyStep}
    (hcomplete : LiveSupportComplete L support)
    (hmem : s ∈ pruneDeletes M L support) :
    ∃ d, s = ApplyStep.delete d ∧ deleteSafe L d := by
  unfold pruneDeletes at hmem
  rw [List.mem_mergeSort, List.mem_filterMap] at hmem
  rcases hmem with ⟨d, _hd, hprod⟩
  by_cases hM : M.contains d = true
  · simp [hM] at hprod
  · have hMfalse : M.contains d = false := by
      cases h : M.contains d <;> simp [h] at hM ⊢
    by_cases hL : L.contains d = true
    · by_cases hcheck : noLiveReferencesIn L support d = true
      · simp [hMfalse, hL, hcheck] at hprod
        subst hprod
        exact ⟨d, rfl, noLiveReferencesIn_deleteSafe hcomplete hcheck⟩
      · have hcheckFalse : noLiveReferencesIn L support d = false := by
          cases h : noLiveReferencesIn L support d <;> simp [h] at hcheck ⊢
        simp [hMfalse, hL, hcheckFalse] at hprod
    · have hLfalse : L.contains d = false := by
        cases h : L.contains d <;> simp [h] at hL ⊢
      simp [hMfalse, hLfalse] at hprod

theorem pruneDeletes_deleteSafe
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ pruneDeletes M L support) :
    deleteSafe L d := by
  rcases pruneDeletes_emits_only_safe_deletes hcomplete hmem with ⟨d', hstep, hsafe⟩
  cases hstep
  exact hsafe

theorem diffPrune_deleteSafe
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ diffPrune M L support) :
    deleteSafe L d := by
  unfold diffPrune at hmem
  rw [List.mem_append] at hmem
  rcases hmem with hdiff | hprune
  · have hpayload := diff_step_payload_isSome hdiff
    simp [ApplyStep.payload?] at hpayload
  · exact pruneDeletes_deleteSafe hcomplete hprune

/-- T-Delete-safety: any delete emitted by prune-mode diff has no structural
    live referrer at the state where the diff is computed, assuming the finite
    live support supplied to prune generation is complete. -/
theorem t_delete_safety
    {M : Manifest} {L : LiveState} {support : LiveSupport} {d : DocRef}
    (hcomplete : LiveSupportComplete L support)
    (hmem : ApplyStep.delete d ∈ diffPrune M L support) :
    ∀ referrer : DocRef, ¬ liveReferences L referrer d :=
  diffPrune_deleteSafe hcomplete hmem

/-- `diff M L` is sorted by `applyOrder`: any step at position `i` has
    `applyOrder` no greater than any step at position `j ≥ i`. Consumed
    by the reference-closure argument for `apply_preserves_wellFormed`
    by the reference-closure proof in `ApplyProperties`. -/
lemma diff_sorted_by_applyOrder (M : Manifest) (L : LiveState)
    (i j : Nat) (hij : i ≤ j) (hj : j < (diff M L).length) :
    ((diff M L).get ⟨i, Nat.lt_of_le_of_lt hij hj⟩).target.collection.applyOrder
      ≤ ((diff M L).get ⟨j, hj⟩).target.collection.applyOrder := by
  rcases Nat.lt_or_eq_of_le hij with hlt | heq
  · -- Strict inequality: use pairwise_iff_getElem.
    have hpw := diff_pairwise_le M L
    have hi : i < (diff M L).length := Nat.lt_of_le_of_lt hij hj
    have hle := (List.pairwise_iff_getElem.mp hpw) i j hi hj hlt
    -- hle : ApplyStep.le (diff M L)[i] (diff M L)[j] = true
    unfold ApplyStep.le at hle
    simp only [List.get_eq_getElem]
    rcases DocRef.le_elim hle with hrank | ⟨heq, _⟩
    · exact Nat.le_of_lt hrank
    · exact Nat.le_of_eq heq
  · -- i = j; reduce to reflexivity.
    subst heq
    exact Nat.le_refl _


end ApplyReconcile
