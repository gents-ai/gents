import Proofs.ApplyReconcile.ApplyProperties

/-!
# Apply/Reconcile Prefix Semantics

Partial apply semantics for crash/retry reasoning. A partial apply is any
prefix of the sorted `diff M L`; retrying starts from the state produced by
that prefix and recomputes `diff`.
-/

namespace ApplyReconcile

/-- A concrete partial apply: some prefix of the full diff. -/
structure ApplyPrefix (M : Manifest) (L : LiveState) where
  steps    : List ApplyStep
  isPrefix : List.IsPrefix steps (diff M L)

namespace ApplyPrefix

/-- State reached after applying this prefix. -/
def state {M : Manifest} {L : LiveState} (p : ApplyPrefix M L) : LiveState :=
  applyAll L p.steps

@[simp]
lemma state_live {M : Manifest} {L : LiveState} (p : ApplyPrefix M L) :
    p.state.live = L.live :=
  apply_preserves_live L p.steps

end ApplyPrefix

/-- The desired projection agrees with the manifest wherever the manifest
    declares a document. Live-only documents outside the manifest are not
    deleted by apply and are intentionally outside this predicate. -/
def ManifestRealized (M : Manifest) (L : LiveState) : Prop :=
  ∀ d : DocRef, ∀ f, M.docs d = some f → L.desired d = some f

/-- Product-facing corollary scoped to documents that have already been written
    by the current prefix. The stronger invariant is `applyPrefix_wellFormed`,
    whose first conjunct covers every document in the prefix state. -/
def PrefixReferrersClosed (pref : List ApplyStep) (L : LiveState) : Prop :=
  ∀ s : ApplyStep, s ∈ pref → ∀ f, L.desired s.target = some f →
    ∀ r ∈ referencesOf f, L.contains r = true

/-- Every applied prefix preserves runtime-owned/live fields. -/
theorem applyPrefix_preserves_live
    {M : Manifest} {L : LiveState} (p : ApplyPrefix M L) :
    p.state.live = L.live :=
  p.state_live

/-- Every applied prefix of a well-formed apply remains well-formed. -/
theorem applyPrefix_wellFormed
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (p : ApplyPrefix M L) :
    p.state.WellFormed :=
  apply_preserves_wellFormed hM hL p.steps p.isPrefix

/-- Every already-written referrer in an applied prefix has all of its
    references present in the prefix state. -/
theorem applyPrefix_referrersClosed
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (p : ApplyPrefix M L) :
    PrefixReferrersClosed p.steps p.state := by
  intro s _hs f hf r hr
  exact (applyPrefix_wellFormed hM hL p).1 s.target f hf r hr

/-- Complete apply realizes the manifest's desired projection. -/
theorem apply_realizes_manifest_desired
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed) :
    ManifestRealized M (applyAll L (diff M L)) :=
  apply_realizes_manifest hM hL

/-- Retrying from any applied prefix converges back to the manifest's desired
    projection after recomputing `diff` from the prefix state. -/
theorem retry_after_prefix_realizes_manifest
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (p : ApplyPrefix M L) :
    ManifestRealized M (applyAll p.state (diff M p.state)) :=
  apply_realizes_manifest hM (applyPrefix_wellFormed hM hL p)

/-- Retry convergence also preserves the runtime/live projection from the
    original state across both the failed prefix and the retry pass. -/
theorem retry_after_prefix_preserves_live
    {M : Manifest} {L : LiveState} (p : ApplyPrefix M L) :
    (applyAll p.state (diff M p.state)).live = L.live := by
  rw [apply_preserves_live p.state (diff M p.state)]
  exact p.state_live

/-- If the live state's desired projection already realizes the manifest,
    the recomputed diff is empty. -/
theorem diff_eq_nil_of_manifestRealized
    {M : Manifest} {L : LiveState}
    (hrealized : ManifestRealized M L) :
    diff M L = [] := by
  unfold diff
  let step? : DocRef → Option ApplyStep := fun d =>
    match M.docs d, L.desired d with
    | some f, none     => some (ApplyStep.create d f)
    | some f, some g   => if f = g then none else some (ApplyStep.update d f)
    | none,   _        => none
  have hfilter : M.support.toList.filterMap step? = [] := by
    apply List.eq_nil_iff_forall_not_mem.mpr
    intro s hs
    rw [List.mem_filterMap] at hs
    obtain ⟨d, hd_mem, hd_step⟩ := hs
    have hd_support : d ∈ M.support := (Finset.mem_toList).mp hd_mem
    have hd_some : (M.docs d).isSome = true := (M.support_iff d).mp hd_support
    obtain ⟨f, hf⟩ := Option.isSome_iff_exists.mp hd_some
    have hdesired : L.desired d = some f := hrealized d f hf
    have hstep_none : step? d = none := by
      simp [step?, hf, hdesired]
    rw [hstep_none] at hd_step
    simp at hd_step
  change (M.support.toList.filterMap step?).mergeSort ApplyStep.le = []
  rw [hfilter]
  exact List.mergeSort_nil

/-- A converged state is an apply fixed point: rerunning diff/apply is a
    no-op. -/
theorem apply_idempotent_of_manifestRealized
    {M : Manifest} {L : LiveState}
    (hrealized : ManifestRealized M L) :
    applyAll L (diff M L) = L := by
  rw [diff_eq_nil_of_manifestRealized hrealized]
  rfl

/-- After a complete well-formed apply, rerunning diff/apply is idempotent. -/
theorem apply_idempotent_after_convergence
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed) :
    let L' := applyAll L (diff M L)
    applyAll L' (diff M L') = L' := by
  intro L'
  exact apply_idempotent_of_manifestRealized (apply_realizes_manifest_desired hM hL)

end ApplyReconcile
