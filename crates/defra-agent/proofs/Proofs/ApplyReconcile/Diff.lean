import Proofs.ApplyReconcile.Manifest

/-!
# Apply/Reconcile Diff

Apply-step vocabulary, diff construction, and ordering lemmas.
-/

namespace ApplyReconcile

/-- A single write landing in the DB from the apply agent.
    By construction carries only `DesiredFields` — no `LiveFields`
    constructor exists, which is the Lean-side restatement of the
    Rust `DesiredFields` bound on the apply boundary. -/
inductive ApplyStep where
  | create (d : DocRef) (f : DesiredFields)
  | update (d : DocRef) (f : DesiredFields)

namespace ApplyStep

def target : ApplyStep → DocRef
  | .create d _ => d
  | .update d _ => d

def payload : ApplyStep → DesiredFields
  | .create _ f => f
  | .update _ f => f

/-- Step-level comparator: orders by target's DocRef ordering.
    `Bool`-valued so it can drive `List.mergeSort`. -/
def le (a b : ApplyStep) : Bool := DocRef.le a.target b.target

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
