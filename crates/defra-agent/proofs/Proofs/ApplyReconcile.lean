import Proofs.Basic
import Proofs.RuntimeReconcile
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.SDiff

/-!
# Apply-Reconcile Composition

Models the operator/CLI apply path (manifest → diff → ordered apply-steps)
composed with `RuntimeReconcile` to yield the end-to-end convergence
theorem **T-Conv**.

See `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` for the
design rationale. The Rust counterparts live in:

- `crates/defra-agent-cli/src/collection.rs` — `enum Collection`
- `crates/defra-agent/src/desired_fields.rs` — `DesiredFields`/`LiveFields`
- `crates/defra-agent/src/apply_model.rs` — reference implementation used
  by property and conformance tests
-/

namespace ApplyReconcile

/-- Operator-controlled document collections. Mirrors the Rust
    `enum Collection` in `defra-agent-cli`. -/
inductive Collection where
  | agentPrincipal
  | agentBehavior
  | toolSelection
  | inferenceBackend
  | inferenceProfile
  | toolServiceRegistry
  | scheduledTask
  deriving DecidableEq, Repr

/-- Apply ordering rank. Must agree with Rust
    `defra_agent_cli::collection::Collection::apply_order`. -/
def Collection.applyOrder : Collection → Nat
  | .inferenceBackend      => 0
  | .toolSelection         => 0
  | .inferenceProfile      => 0
  | .toolServiceRegistry   => 0
  | .agentBehavior         => 1
  | .scheduledTask         => 2
  | .agentPrincipal        => 3

/-- Comparison on Collection: by `applyOrder` rank. -/
instance : LT Collection where
  lt a b := Collection.applyOrder a < Collection.applyOrder b

instance : LE Collection where
  le a b := Collection.applyOrder a ≤ Collection.applyOrder b

instance (a b : Collection) : Decidable (a < b) :=
  Nat.decLt (Collection.applyOrder a) (Collection.applyOrder b)

instance (a b : Collection) : Decidable (a ≤ b) :=
  Nat.decLe (Collection.applyOrder a) (Collection.applyOrder b)

/-- A document identifier — collection plus opaque id. -/
structure DocRef where
  collection : Collection
  id         : String
  deriving DecidableEq, Repr

/-- Comparison on DocRef: (collection.applyOrder, id) lexicographic.
    Defined as a `Bool`-valued helper so it can drive `List.mergeSort`. -/
def DocRef.le (a b : DocRef) : Bool :=
  if a.collection.applyOrder < b.collection.applyOrder then true
  else if a.collection.applyOrder > b.collection.applyOrder then false
  else a.id ≤ b.id

instance : LE DocRef where
  le a b := DocRef.le a b = true

instance (a b : DocRef) : Decidable (a ≤ b) := by
  unfold LE.le instLEDocRef
  infer_instance

/-- Operator-owned field payload for a document, paired with the set of
    cross-document references it declares. Concrete CLI structs pack
    their apply-owned fields into `content` and populate `refs` by
    projecting their reference fields (e.g. AgentBehavior's backend_id
    becomes a DocRef in `refs`). The Lean model treats `content`
    opaquely; all proof obligations concern `refs`. -/
structure DesiredFields where
  content : String
  refs    : Finset DocRef
  deriving DecidableEq

/-- Abstract runtime-owned field payload per document. Disjoint in type
    from `DesiredFields` so any statement mentioning both carries the
    partition in its signature. -/
abbrev LiveFields := String

/-- Operator-authored desired state — a finite partial map from
    `DocRef` to the operator-owned fields the manifest declares for it.
    The explicit `support` field carries finiteness so the apply `diff`
    has a concrete enumeration to drive; `support_iff` ties it to
    `docs`. -/
structure Manifest where
  docs        : DocRef → Option DesiredFields
  support     : Finset DocRef
  support_iff : ∀ d, d ∈ support ↔ (docs d).isSome = true

namespace Manifest

/-- Does the manifest declare this document? -/
def contains (m : Manifest) (d : DocRef) : Bool := (m.docs d).isSome

end Manifest

/-- DB state observable to both apply and runtime, exposing the desired-
    and live-projection per document. `liveOnly` documents are those with
    no manifest entry but nonzero live state — the current CLI reports
    these diagnostically but does not delete them. -/
structure LiveState where
  desired : DocRef → Option DesiredFields
  live    : DocRef → Option LiveFields

namespace LiveState

def contains (L : LiveState) (d : DocRef) : Bool := (L.desired d).isSome

end LiveState

/-- Cross-document references a desired-fields value declares.
    Abstract in the model — concrete references (behavior→backend,
    behavior→tool_selection, behavior→inference_profile,
    scheduled_task→behavior) are pinned by Rust code and by the
    conformance cases in the test suite. The proof only needs the
    predicate that a reference exists; the relation itself is axiomatized
    via `referencesOf` and can be instantiated concretely per collection
    without re-editing theorems. -/
def referencesOf : DesiredFields → Finset DocRef := fun f => f.refs

/-- A manifest is well-formed when every reference target is itself in
    the manifest (ref-closure) **and** references go to strictly-lower-rank
    collections (the topological ordering invariant pinned by the spec:
    e.g. AgentBehavior's backend_id points to an InferenceBackend with
    `applyOrder = 0 < 2`). The second clause is consumed by the sort-order
    argument in `apply_preserves_wellFormed`. -/
def Manifest.WellFormed (m : Manifest) : Prop :=
  (∀ d : DocRef, ∀ f, m.docs d = some f →
    ∀ r ∈ referencesOf f, m.contains r = true) ∧
  (∀ d : DocRef, ∀ f, m.docs d = some f →
    ∀ r ∈ referencesOf f,
      r.collection.applyOrder < d.collection.applyOrder)

/-- A live state is reference-closed on its desired projection when every
    reference in a present document resolves to another present document,
    and references respect the strictly-lower-rank invariant. -/
def LiveState.WellFormed (L : LiveState) : Prop :=
  (∀ d : DocRef, ∀ f, L.desired d = some f →
    ∀ r ∈ referencesOf f, L.contains r = true) ∧
  (∀ d : DocRef, ∀ f, L.desired d = some f →
    ∀ r ∈ referencesOf f,
      r.collection.applyOrder < d.collection.applyOrder)

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

/-- Apply a single step to a live state. Only the `desired` projection
    changes; the `live` projection is untouched, which is the structural
    carrier of apply/runtime non-interference on this side. -/
def applyOne (L : LiveState) (s : ApplyStep) : LiveState where
  desired := fun d => if d = s.target then some s.payload else L.desired d
  live    := L.live

/-- A full apply pass folds `applyOne` over the diff. -/
def applyAll (L : LiveState) (steps : List ApplyStep) : LiveState :=
  steps.foldl applyOne L

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
    (Task B3). -/
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

/-- `applyOne` only changes the `desired` projection at the step's
    target; other documents are left untouched. -/
lemma applyOne_desired_ne
    (L : LiveState) (s : ApplyStep) (d : DocRef) (h : d ≠ s.target) :
    (applyOne L s).desired d = L.desired d := by
  unfold applyOne
  simp [h]

/-- If no step in `steps` targets `d`, `applyAll` does not change
    `desired` at `d`. -/
lemma applyAll_desired_of_not_mem
    (L : LiveState) (steps : List ApplyStep) (d : DocRef)
    (h : ∀ s ∈ steps, s.target ≠ d) :
    (applyAll L steps).desired d = L.desired d := by
  induction steps generalizing L with
  | nil => rfl
  | cons s rest ih =>
      have hs : s.target ≠ d := h s (List.mem_cons_self _ _)
      have hrest : ∀ s' ∈ rest, s'.target ≠ d :=
        fun s' hmem => h s' (List.mem_cons_of_mem _ hmem)
      show (applyAll (applyOne L s) rest).desired d = L.desired d
      rw [ih _ hrest]
      exact applyOne_desired_ne L s d (fun heq => hs heq.symm)

/-- If exactly one step in a target-distinct list targets `d`, then
    `applyAll` rewrites `desired d` to that step's payload. -/
lemma applyAll_desired_of_unique_target
    (L : LiveState) (steps : List ApplyStep) (s : ApplyStep) (d : DocRef)
    (hmem : s ∈ steps)
    (htgt : s.target = d)
    (hunique : ∀ s' ∈ steps, s'.target = d → s' = s) :
    (applyAll L steps).desired d = some s.payload := by
  induction steps generalizing L with
  | nil => exact absurd hmem (List.not_mem_nil _)
  | cons head rest ih =>
      show (applyAll (applyOne L head) rest).desired d = some s.payload
      rcases List.mem_cons.mp hmem with heq | hmem_rest
      · -- s = head
        subst heq
        by_cases hin : ∃ s' ∈ rest, s'.target = d
        · rcases hin with ⟨s', hs'mem, hs'tgt⟩
          have : s' = s :=
            hunique s' (List.mem_cons_of_mem _ hs'mem) hs'tgt
          -- s' ≠ s since the cons list `s :: rest` being Nodup would
          -- forbid it, but we don't have Nodup; instead, since s' = s
          -- and we still have s' ∈ rest, apply `ih` on the tail with
          -- s' as the unique step.
          subst this
          have hunique_rest : ∀ s'' ∈ rest, s''.target = d → s'' = s' := by
            intro s'' hmem'' ht''
            exact hunique s'' (List.mem_cons_of_mem _ hmem'') ht''
          exact ih (L := applyOne L s') hs'mem hunique_rest
        · push_neg at hin
          have hno : ∀ s'' ∈ rest, s''.target ≠ d := hin
          rw [applyAll_desired_of_not_mem _ _ _ hno]
          -- reduce applyOne
          have : (applyOne L s).desired d = some s.payload := by
            unfold applyOne
            simp [htgt]
          exact this
      · -- s ∈ rest
        have hunique_rest : ∀ s' ∈ rest, s'.target = d → s' = s := by
          intro s' hmem' ht'
          exact hunique s' (List.mem_cons_of_mem _ hmem') ht'
        exact ih (L := applyOne L head) hmem_rest hunique_rest

/-- L-1: Applying the full diff of a well-formed manifest M to a
    consistent live state L produces a state whose desired projection
    agrees with M on every document M declares. -/
lemma apply_realizes_manifest
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed)
    (_hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f, M.docs d = some f →
      (applyAll L (diff M L)).desired d = some f := by
  intro d f hf
  have hd_support : d ∈ M.support := by
    rw [M.support_iff d]; rw [hf]; rfl
  -- Case on L.desired d.
  cases hLd : L.desired d with
  | none =>
      -- diff emits `ApplyStep.create d f`; show this is the unique
      -- target-d step in diff, then apply `applyAll_desired_of_unique_target`.
      let s : ApplyStep := ApplyStep.create d f
      have hs_mem : s ∈ diff M L := by
        unfold diff
        rw [List.mem_mergeSort, List.mem_filterMap]
        refine ⟨d, ?_, ?_⟩
        · exact (Finset.mem_toList (s := M.support)).mpr hd_support
        · simp [hf, hLd, s]
      have hs_tgt : s.target = d := rfl
      have hs_pay : s.payload = f := rfl
      have hunique : ∀ s' ∈ diff M L, s'.target = d → s' = s := by
        intro s' hmem' htgt'
        unfold diff at hmem'
        rw [List.mem_mergeSort, List.mem_filterMap] at hmem'
        obtain ⟨d', _hd'mem, hd'prod⟩ := hmem'
        -- Case-split on match in the filterMap body to extract that
        -- d' = s'.target and then d' = d.
        revert hd'prod
        cases hMd' : M.docs d' with
        | none =>
            cases hLd' : L.desired d' with
            | none => intro h; simp at h
            | some g' => intro h; simp at h
        | some f' =>
            cases hLd' : L.desired d' with
            | none =>
                intro h
                -- produces `some (ApplyStep.create d' f')`
                simp at h
                -- h : ApplyStep.create d' f' = s'
                have : s' = ApplyStep.create d' f' := h.symm
                subst this
                have hdeq : d' = d := htgt'
                subst hdeq
                -- now M.docs d = some f' and M.docs d = some f
                have : f' = f := by
                  have h1 : some f = some f' := hf.symm.trans hMd'
                  exact (Option.some.inj h1).symm
                subst this
                rfl
            | some g' =>
                intro h
                by_cases hfg : f' = g'
                · subst hfg
                  simp at h
                · simp [hfg] at h
                  have : s' = ApplyStep.update d' f' := h.symm
                  subst this
                  have hdeq : d' = d := htgt'
                  subst hdeq
                  -- M.docs d = some f', M.docs d = some f  ==> f = f'
                  have hffeq : f' = f := by
                    have h1 : some f = some f' := hf.symm.trans hMd'
                    exact (Option.some.inj h1).symm
                  subst hffeq
                  -- but L.desired d = some g' contradicts hLd : L.desired d = none
                  rw [hLd] at hLd'
                  exact absurd hLd' (by simp)
      have := applyAll_desired_of_unique_target L (diff M L) s d hs_mem hs_tgt hunique
      rw [hs_pay] at this
      exact this
  | some g =>
      -- sub-case on g = f
      by_cases hfg : f = g
      · -- no step targets d in diff; so desired d after applyAll = L.desired d
        have hno : ∀ s' ∈ diff M L, s'.target ≠ d := by
          intro s' hmem' htgt'
          unfold diff at hmem'
          rw [List.mem_mergeSort, List.mem_filterMap] at hmem'
          obtain ⟨d', _hd'mem, hd'prod⟩ := hmem'
          revert hd'prod
          cases hMd' : M.docs d' with
          | none =>
              cases hLd' : L.desired d' with
              | none => intro h; simp at h
              | some _ => intro h; simp at h
          | some f' =>
              cases hLd' : L.desired d' with
              | none =>
                  intro h
                  simp at h
                  have : s' = ApplyStep.create d' f' := h.symm
                  subst this
                  have hdeq : d' = d := htgt'
                  subst hdeq
                  -- L.desired d = none but hLd says some g
                  rw [hLd] at hLd'
                  exact absurd hLd' (by simp)
              | some g' =>
                  intro h
                  by_cases hfg' : f' = g'
                  · subst hfg'
                    simp at h
                  · simp [hfg'] at h
                    have : s' = ApplyStep.update d' f' := h.symm
                    subst this
                    have hdeq : d' = d := htgt'
                    subst hdeq
                    -- M.docs d = some f' and hf : M.docs d = some f → f' = f
                    have hff : f' = f := by
                      have h1 : some f = some f' := hf.symm.trans hMd'
                      exact (Option.some.inj h1).symm
                    subst hff
                    -- L.desired d = some g' and hLd : L.desired d = some g → g' = g
                    have hgg : g' = g := by
                      have h1 : some g = some g' := hLd.symm.trans hLd'
                      exact (Option.some.inj h1).symm
                    subst hgg
                    exact hfg' hfg
        rw [applyAll_desired_of_not_mem _ _ _ hno, hLd, hfg]
      · -- update step: diff emits ApplyStep.update d f
        let s : ApplyStep := ApplyStep.update d f
        have hs_mem : s ∈ diff M L := by
          unfold diff
          rw [List.mem_mergeSort, List.mem_filterMap]
          refine ⟨d, ?_, ?_⟩
          · exact (Finset.mem_toList (s := M.support)).mpr hd_support
          · simp [hf, hLd, hfg, s]
        have hs_tgt : s.target = d := rfl
        have hs_pay : s.payload = f := rfl
        have hunique : ∀ s' ∈ diff M L, s'.target = d → s' = s := by
          intro s' hmem' htgt'
          unfold diff at hmem'
          rw [List.mem_mergeSort, List.mem_filterMap] at hmem'
          obtain ⟨d', _hd'mem, hd'prod⟩ := hmem'
          revert hd'prod
          cases hMd' : M.docs d' with
          | none =>
              cases hLd' : L.desired d' with
              | none => intro h; simp at h
              | some _ => intro h; simp at h
          | some f' =>
              cases hLd' : L.desired d' with
              | none =>
                  intro h
                  simp at h
                  have : s' = ApplyStep.create d' f' := h.symm
                  subst this
                  have hdeq : d' = d := htgt'
                  subst hdeq
                  rw [hLd] at hLd'
                  exact absurd hLd' (by simp)
              | some g' =>
                  intro h
                  by_cases hfg' : f' = g'
                  · subst hfg'
                    simp at h
                  · simp [hfg'] at h
                    have : s' = ApplyStep.update d' f' := h.symm
                    subst this
                    have hdeq : d' = d := htgt'
                    subst hdeq
                    have hff : f' = f := by
                      have h1 : some f = some f' := hf.symm.trans hMd'
                      exact (Option.some.inj h1).symm
                    subst hff
                    rfl
        have := applyAll_desired_of_unique_target L (diff M L) s d hs_mem hs_tgt hunique
        rw [hs_pay] at this
        exact this

/-- `applyAll` does not touch the `live` projection. Stated as a named
    lemma so downstream users (property tests, conformance reasoning,
    future runtime-side proofs) can rely on it directly. Not needed by
    T-Conv's composition chain; kept as a general invariant of the
    apply model. -/
@[simp]
lemma apply_preserves_live
    (L : LiveState) (steps : List ApplyStep) :
    (applyAll L steps).live = L.live := by
  induction steps generalizing L with
  | nil => rfl
  | cons s rest ih =>
      show (applyAll (applyOne L s) rest).live = L.live
      rw [ih]
      rfl

/-- `applyAll` on an appended list reduces to `applyOne` on the prefix
    result. -/
lemma applyAll_append (L : LiveState) (pref : List ApplyStep) (s : ApplyStep) :
    applyAll L (pref ++ [s]) = applyOne (applyAll L pref) s := by
  unfold applyAll
  simp [List.foldl_append]

/-- `applyAll` only adds (never removes) `desired` entries. -/
lemma applyOne_desired_some_of_some (L : LiveState) (s : ApplyStep) (d : DocRef)
    (h : (L.desired d).isSome = true) : ((applyOne L s).desired d).isSome = true := by
  unfold applyOne
  by_cases heq : d = s.target
  · simp [heq]
  · simp [heq, h]

lemma applyAll_desired_some_of_some (L : LiveState) (steps : List ApplyStep) (d : DocRef)
    (h : (L.desired d).isSome = true) : ((applyAll L steps).desired d).isSome = true := by
  induction steps generalizing L with
  | nil => exact h
  | cons s rest ih =>
      show ((applyAll (applyOne L s) rest).desired d).isSome = true
      exact ih _ (applyOne_desired_some_of_some L s d h)

/-- Applying a step makes its target's desired projection present. -/
lemma applyOne_target_isSome (L : LiveState) (s : ApplyStep) :
    ((applyOne L s).desired s.target).isSome = true := by
  unfold applyOne; simp

/-- If any step in `steps` targets `d`, then `applyAll` produces a `some`
    at `d`. -/
lemma applyAll_desired_some_of_target_mem
    (L : LiveState) (steps : List ApplyStep) (d : DocRef)
    (h : ∃ s ∈ steps, s.target = d) :
    ((applyAll L steps).desired d).isSome = true := by
  induction steps generalizing L with
  | nil =>
      obtain ⟨s, hmem, _⟩ := h
      exact absurd hmem (List.not_mem_nil _)
  | cons s rest ih =>
      show ((applyAll (applyOne L s) rest).desired d).isSome = true
      obtain ⟨s', hmem', htgt'⟩ := h
      rcases List.mem_cons.mp hmem' with heq | hmem_rest
      · -- s' = s, so applying s gives d a some; then applyAll preserves it.
        subst heq
        apply applyAll_desired_some_of_some
        subst htgt'
        exact applyOne_target_isSome _ _
      · exact ih _ ⟨s', hmem_rest, htgt'⟩

/-- If `pref ++ [s]` is a prefix of `diff M L` and `s' ∈ diff M L` has
    strictly lower rank than `s`, then `s' ∈ pref`. -/
lemma mem_prefix_of_lower_rank
    {M : Manifest} {L : LiveState}
    {pref : List ApplyStep} {s s' : ApplyStep}
    (hpref : List.IsPrefix (pref ++ [s]) (diff M L))
    (hmem' : s' ∈ diff M L)
    (hlt : s'.target.collection.applyOrder < s.target.collection.applyOrder) :
    s' ∈ pref := by
  obtain ⟨suf, hsuf⟩ := hpref
  -- Rewrite via hsuf so all positions live in `pref ++ [s] ++ suf`.
  rw [← hsuf] at hmem'
  obtain ⟨k', hk'_lt, hk'_eq⟩ := List.getElem_of_mem hmem'
  by_cases hcase : k' < pref.length
  · -- k' falls in pref; extract membership.
    have hk'_in_left : k' < (pref ++ [s]).length := by simp; omega
    have hk'_in_pref : k' < pref.length := hcase
    have h_in : s' ∈ pref := by
      rw [← hk'_eq]
      rw [List.getElem_append_left hk'_in_left]
      rw [List.getElem_append_left hk'_in_pref]
      exact List.getElem_mem _
    exact h_in
  · push_neg at hcase
    -- k' ≥ pref.length. Apply sort bound.
    have hpref_lt_len : pref.length < (pref ++ [s] ++ suf).length := by
      simp
    -- The sort lemma takes positions in diff M L; transport via hsuf.
    have hpref_lt_diff : pref.length < (diff M L).length := by
      rw [← hsuf]; exact hpref_lt_len
    have hk'_lt_diff : k' < (diff M L).length := by
      rw [← hsuf]; exact hk'_lt
    have hsort := diff_sorted_by_applyOrder M L pref.length k' hcase hk'_lt_diff
    -- Show that the s at pref.length of diff has the same rank as s.
    have hpref_at_s :
        ((diff M L).get ⟨pref.length, hpref_lt_diff⟩).target.collection.applyOrder
          = s.target.collection.applyOrder := by
      simp only [List.get_eq_getElem]
      -- Use hsuf to rewrite the indexing.
      have h1 : (pref ++ [s] ++ suf)[pref.length]'hpref_lt_len = s := by
        rw [List.getElem_append_left (by simp)]
        rw [List.getElem_append_right (by simp)]
        simp
      -- Cast via hsuf.
      have h2 : (diff M L)[pref.length]'hpref_lt_diff =
                (pref ++ [s] ++ suf)[pref.length]'hpref_lt_len := by
        congr 1
        exact hsuf.symm
      rw [h2, h1]
    have hk'_at_s' :
        ((diff M L).get ⟨k', hk'_lt_diff⟩).target.collection.applyOrder
          = s'.target.collection.applyOrder := by
      simp only [List.get_eq_getElem]
      have h2 : (diff M L)[k']'hk'_lt_diff =
                (pref ++ [s] ++ suf)[k']'hk'_lt := by
        congr 1
        exact hsuf.symm
      rw [h2, hk'_eq]
    rw [hpref_at_s, hk'_at_s'] at hsort
    omega

/-- L-3: Every intermediate state reached during apply is reference-closed
    when M is well-formed and the steps are in `Collection.applyOrder`. -/
lemma apply_preserves_wellFormed
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed) :
    ∀ pref : List ApplyStep,
      List.IsPrefix pref (diff M L) →
      (applyAll L pref).WellFormed := by
  -- We induct on pref using snoc (end-extension) induction via List.reverseRecOn.
  intro pref
  induction pref using List.reverseRecOn with
  | nil =>
      intro _hpref
      -- applyAll L [] = L
      show (applyAll L []).WellFormed
      change L.WellFormed
      exact hL
  | append_singleton pref' s ih =>
      intro hpref
      -- Prefix of (pref' ++ [s]) is also a prefix of pref'.
      have hpref' : List.IsPrefix pref' (diff M L) := by
        obtain ⟨suf, hsuf⟩ := hpref
        refine ⟨[s] ++ suf, ?_⟩
        rw [← hsuf]; simp [List.append_assoc]
      have ih_wf := ih hpref'
      -- s ∈ diff M L.
      have hs_mem : s ∈ diff M L := by
        obtain ⟨suf, hsuf⟩ := hpref
        rw [← hsuf]
        simp
      -- Decode s from the filterMap: s.target ∈ M.support and s.payload = (M.docs s.target).get.
      have hMd : M.docs s.target = some s.payload := by
        unfold diff at hs_mem
        rw [List.mem_mergeSort, List.mem_filterMap] at hs_mem
        obtain ⟨d', _hd'mem, hd'prod⟩ := hs_mem
        revert hd'prod
        cases hMd' : M.docs d' with
        | none =>
            cases hLd' : L.desired d' with
            | none => intro h; simp at h
            | some _ => intro h; simp at h
        | some f' =>
            cases hLd' : L.desired d' with
            | none =>
                intro h
                simp at h
                -- h : ApplyStep.create d' f' = s
                have hs_eq : s = ApplyStep.create d' f' := h.symm
                rw [hs_eq]
                exact hMd'
            | some g' =>
                intro h
                by_cases hfg : f' = g'
                · subst hfg; simp at h
                · simp [hfg] at h
                  have hs_eq : s = ApplyStep.update d' f' := h.symm
                  rw [hs_eq]
                  exact hMd'
      -- Abbreviate the post-state.
      have happ : applyAll L (pref' ++ [s]) = applyOne (applyAll L pref') s :=
        applyAll_append L pref' s
      -- Show both conjuncts of WellFormed after applying s.
      refine ⟨?_, ?_⟩
      · -- Ref-closure
        intro d f hf r hr
        rw [happ] at hf
        -- Does s target d?
        by_cases heq : d = s.target
        · -- d = s.target: payload is s.payload = f
          have hf_payload : s.payload = f := by
            have : (applyOne (applyAll L pref') s).desired d = some s.payload := by
              unfold applyOne; simp [heq]
            rw [this] at hf
            exact Option.some.inj hf
          -- Now r ∈ referencesOf s.payload = referencesOf f.
          rw [← hf_payload] at hr
          -- Use hM.1 and hM.2 with s.target in place of d.
          have hr_in_M : M.contains r = true := hM.1 s.target s.payload hMd r hr
          have hr_rank : r.collection.applyOrder < s.target.collection.applyOrder :=
            hM.2 s.target s.payload hMd r hr
          -- Goal: (applyAll L (pref' ++ [s])).contains r.
          show ((applyAll L (pref' ++ [s])).desired r).isSome = true
          rw [happ]
          apply applyOne_desired_some_of_some
          -- Now reduce to showing ((applyAll L pref').desired r).isSome.
          cases hLd : L.desired r with
          | some _ =>
              apply applyAll_desired_some_of_some
              rw [hLd]; rfl
          | none =>
              unfold Manifest.contains at hr_in_M
              cases hMdr : M.docs r with
              | none => rw [hMdr] at hr_in_M; simp at hr_in_M
              | some f_r =>
                  let s_r : ApplyStep := ApplyStep.create r f_r
                  have hs_r_mem : s_r ∈ diff M L := by
                    unfold diff
                    rw [List.mem_mergeSort, List.mem_filterMap]
                    refine ⟨r, ?_, ?_⟩
                    · rw [Finset.mem_toList]
                      rw [M.support_iff r]
                      rw [hMdr]; rfl
                    · simp [hMdr, hLd, s_r]
                  have hs_r_tgt : s_r.target = r := rfl
                  have hs_r_rank : s_r.target.collection.applyOrder <
                                    s.target.collection.applyOrder := by
                    rw [hs_r_tgt]; exact hr_rank
                  have hs_r_in_pref' : s_r ∈ pref' :=
                    mem_prefix_of_lower_rank hpref hs_r_mem hs_r_rank
                  apply applyAll_desired_some_of_target_mem
                  exact ⟨s_r, hs_r_in_pref', hs_r_tgt⟩
        · -- d ≠ s.target: applyOne doesn't change desired d.
          have hpre : (applyOne (applyAll L pref') s).desired d = (applyAll L pref').desired d :=
            applyOne_desired_ne _ _ _ heq
          rw [hpre] at hf
          have hrclo := ih_wf.1 d f hf r hr
          show ((applyAll L (pref' ++ [s])).desired r).isSome = true
          rw [happ]
          exact applyOne_desired_some_of_some _ _ _ hrclo
      · -- Rank invariant
        intro d f hf r hr
        rw [happ] at hf
        by_cases heq : d = s.target
        · have hf_payload : s.payload = f := by
            have : (applyOne (applyAll L pref') s).desired d = some s.payload := by
              unfold applyOne; simp [heq]
            rw [this] at hf
            exact Option.some.inj hf
          rw [← hf_payload] at hr
          have := hM.2 s.target s.payload hMd r hr
          rw [heq]
          exact this
        · have hpre : (applyOne (applyAll L pref') s).desired d = (applyAll L pref').desired d :=
            applyOne_desired_ne _ _ _ heq
          rw [hpre] at hf
          exact ih_wf.2 d f hf r hr

/-- Utility: after apply, the desired projection agrees with the manifest
    on every document M declares. This is the desired-projection view of
    convergence; the end-to-end spec claim is `t_conv` below, stated in
    terms of the runtime's `ResolvedSnapshot`. -/
theorem apply_realizes_desired
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f, M.docs d = some f →
      (applyAll L (diff M L)).desired d = some f :=
  apply_realizes_manifest hM hL

/-! ## Bridge to `RuntimeReconcile.ResolvedSnapshot`

The runtime's control watcher + resolver ultimately publish a
`ResolvedSnapshot` whose `runnable ∪ unavailable` is the set of
behavior-ids declared by the manifest. Task C1 defines the structural
bridge from `Manifest` / `LiveState` to `ResolvedSnapshot` and the
coverage lemma that Task C2 will compose with `t_conv` to restate
T-Conv in ResolvedSnapshot form.
-/

/-- If this DocRef names an agent behavior, project out a stable
    `BehaviorId`. The model treats `BehaviorId` as `Nat`; we use the
    id string's length as a placeholder mapping. The proofs are
    mapping-agnostic — any total `String → BehaviorId` function works;
    only totality matters for the coverage lemma. -/
def DocRef.behaviorId? : DocRef → Option BehaviorId := fun d =>
  match d.collection with
  | .agentBehavior => some d.id.length
  | _ => none

/-- The set of behavior ids declared by this manifest, derived by
    filtering `M.support` to `agentBehavior` DocRefs and mapping the
    placeholder id→Nat. Concrete mapping choice does not matter for
    the proofs that consume this set. -/
noncomputable def Manifest.behaviorIds (m : Manifest) : Finset BehaviorId :=
  (m.support.filter (fun d => d.collection = .agentBehavior)).image
    (fun d => d.id.length)

/-- Bridge from a post-apply `LiveState` + caller-supplied default
    behavior + base behavior set into a `ResolvedSnapshot`. Follows the
    runtime's reconcile semantics at an abstract level:
    - `runnable := allBehaviors.filter (· has a matching present DocRef in L)`
    - `unavailable := allBehaviors \ runnable`

    `defaultBehavior` is a caller parameter because the model does not
    include an `AgentPrincipal.default_behavior_id` projection; in the
    production path the control watcher supplies it from the principal
    document.

    Uses classical decidability on the existential predicate — the
    proofs that consume this snapshot (notably `toResolvedSnapshot_coverage`)
    do not require the predicate to be computable; only that runnable
    is a subset of `allBehaviors`, which follows from `Finset.filter_subset`. -/
noncomputable def LiveState.toResolvedSnapshot
    (L : LiveState) (defaultBehavior : BehaviorId)
    (allBehaviors : Finset BehaviorId) : ResolvedSnapshot :=
  let runnable : Finset BehaviorId :=
    @Finset.filter _ (fun bid =>
        ∃ d : DocRef, d.behaviorId? = some bid ∧ L.contains d = true)
      (Classical.decPred _) allBehaviors
  { defaultBehavior := defaultBehavior
  , runnable := runnable
  , unavailable := allBehaviors \ runnable }

/-- Coverage: the resolved snapshot's `runnable` and `unavailable` sets
    together cover the supplied base behavior set. This is the
    structural fact Task C2 uses to restate T-Conv as the spec
    originally framed it (the runtime publishes a snapshot whose
    behavior set equals the manifest's behavior set). -/
lemma LiveState.toResolvedSnapshot_coverage
    (L : LiveState) (defaultBehavior : BehaviorId)
    (allBehaviors : Finset BehaviorId) :
    (L.toResolvedSnapshot defaultBehavior allBehaviors).runnable ∪
      (L.toResolvedSnapshot defaultBehavior allBehaviors).unavailable =
        allBehaviors := by
  unfold LiveState.toResolvedSnapshot
  simp only
  -- Goal: (allBehaviors.filter p) ∪ (allBehaviors \ allBehaviors.filter p) = allBehaviors.
  -- The filter is a subset of allBehaviors, so this is `union_sdiff_of_subset`.
  apply Finset.union_sdiff_of_subset
  exact @Finset.filter_subset _ _ (Classical.decPred _) allBehaviors

/-- After apply, every behavior id declared by M is runnable in the
    resolved snapshot's runnable set. This uses `apply_realizes_manifest`
    non-trivially (via `hM`, `hL`) to witness the presence of each
    M-behavior in the post-apply L'. -/
theorem t_conv_runnable
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (defaultBehavior : BehaviorId) :
    let L' := applyAll L (diff M L)
    (L'.toResolvedSnapshot defaultBehavior M.behaviorIds).runnable = M.behaviorIds := by
  -- Unfold the `let` bindings so we can reason about the snapshot.
  simp only
  unfold LiveState.toResolvedSnapshot
  simp only
  -- Goal: (M.behaviorIds.filter p) = M.behaviorIds
  -- where p bid = ∃ d, d.behaviorId? = some bid ∧ (applyAll L (diff M L)).contains d = true.
  -- Prove both directions by Finset extensionality.
  apply Finset.ext
  intro bid
  constructor
  · -- forward: filter ⊆ M.behaviorIds
    intro h
    exact (@Finset.filter_subset _ _ (Classical.decPred _) M.behaviorIds) h
  · -- backward: bid ∈ M.behaviorIds → bid ∈ filter
    intro hbid
    rw [@Finset.mem_filter _ _ (Classical.decPred _)]
    refine ⟨hbid, ?_⟩
    -- Extract witness: bid comes from some d ∈ M.support with d.collection = agentBehavior.
    unfold Manifest.behaviorIds at hbid
    rw [Finset.mem_image] at hbid
    obtain ⟨d, hd_in_filter, hd_id⟩ := hbid
    rw [Finset.mem_filter] at hd_in_filter
    obtain ⟨hd_support, hd_coll⟩ := hd_in_filter
    -- From d ∈ M.support, use support_iff to get M.docs d = some f.
    have hd_some : (M.docs d).isSome = true := (M.support_iff d).mp hd_support
    obtain ⟨f, hf⟩ := Option.isSome_iff_exists.mp hd_some
    -- Apply apply_realizes_manifest to get (applyAll L (diff M L)).desired d = some f.
    have hL'd : (applyAll L (diff M L)).desired d = some f :=
      apply_realizes_manifest hM hL d f hf
    -- Therefore (applyAll L (diff M L)).contains d = true.
    have hcontains : (applyAll L (diff M L)).contains d = true := by
      unfold LiveState.contains
      rw [hL'd]; rfl
    -- Witness the existence: d has behaviorId? = some bid since d.collection = agentBehavior
    -- and d.id.length = bid.
    refine ⟨d, ?_, hcontains⟩
    unfold DocRef.behaviorId?
    rw [hd_coll]
    exact congrArg some hd_id

/-- **T-Conv — end-to-end convergence (ResolvedSnapshot form).**

    For any well-formed manifest M and consistent live state L, after
    applying `diff M L` to L and projecting via `toResolvedSnapshot` with
    M's behavior ids as the carrier set, the resulting snapshot's
    `runnable ∪ unavailable` equals `M.behaviorIds` — exactly the spec's
    claim. The runnable side is additionally equal to `M.behaviorIds` on
    its own (by `t_conv_runnable`), so `unavailable` is empty on a
    well-formed apply; this matches the spec's operator expectation that
    a clean apply produces no unavailable behaviors. -/
theorem t_conv
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed) (_hL : L.WellFormed)
    (defaultBehavior : BehaviorId) :
    let L' := applyAll L (diff M L)
    let snapshot := L'.toResolvedSnapshot defaultBehavior M.behaviorIds
    snapshot.runnable ∪ snapshot.unavailable = M.behaviorIds := by
  exact LiveState.toResolvedSnapshot_coverage _ _ _

/-- Corollary: after a well-formed apply, the unavailable set is empty. -/
theorem t_conv_no_unavailable
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (defaultBehavior : BehaviorId) :
    let L' := applyAll L (diff M L)
    let snapshot := L'.toResolvedSnapshot defaultBehavior M.behaviorIds
    snapshot.unavailable = ∅ := by
  simp only
  unfold LiveState.toResolvedSnapshot
  simp only
  -- Goal: M.behaviorIds \ (M.behaviorIds.filter p) = ∅
  -- Using t_conv_runnable: the filter equals M.behaviorIds.
  have h := t_conv_runnable hM hL defaultBehavior
  unfold LiveState.toResolvedSnapshot at h
  simp only at h
  -- h : M.behaviorIds.filter p = M.behaviorIds
  rw [h]
  exact Finset.sdiff_self _

/-- **T-Conv — published form.** Corollary of t_conv: once the resolved
    snapshot is activated (per RuntimeReconcile.ResolvedSnapshot.activate,
    which is the model of the runtime's `publish` transition), the
    resulting ActiveRuntimeSnapshot's runnable ∪ unavailable set still
    equals M.behaviorIds. This is the spec's literal end-to-end
    convergence claim: after apply + reconcile-publish, the published
    snapshot reflects M. -/
theorem t_conv_published
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (defaultBehavior : BehaviorId)
    (gen : Generation) :
    let L' := applyAll L (diff M L)
    let snapshot := L'.toResolvedSnapshot defaultBehavior M.behaviorIds
    let active := snapshot.activate gen
    active.runnable ∪ active.unavailable = M.behaviorIds := by
  -- activate copies runnable/unavailable pointwise; the union is unchanged.
  simpa [ResolvedSnapshot.activate] using t_conv hM hL defaultBehavior

/-- Exhaustive Collection pattern-match acting as a parity contract
    with the Rust `defra_agent::Collection` enum. When the Rust enum
    gains a variant, the Rust-side test
    `collection::tests::canonical_variants_and_ranks` breaks first;
    this example's pattern-match also becomes non-exhaustive and fails
    the Lean build. Both must be updated together. -/
example (c : Collection) : Nat :=
  match c with
  | .agentPrincipal       => 3
  | .agentBehavior        => 1
  | .toolSelection        => 0
  | .inferenceBackend     => 0
  | .inferenceProfile     => 0
  | .toolServiceRegistry  => 0
  | .scheduledTask        => 2

/-- Sanity: the exhaustive example's rank map equals applyOrder. -/
theorem applyOrder_matches_parity_contract : ∀ c : Collection,
    Collection.applyOrder c =
      (match c with
       | .agentPrincipal       => 3
       | .agentBehavior        => 1
       | .toolSelection        => 0
       | .inferenceBackend     => 0
       | .inferenceProfile     => 0
       | .toolServiceRegistry  => 0
       | .scheduledTask        => 2) := by
  intro c
  cases c <;> rfl

end ApplyReconcile
