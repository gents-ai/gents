import Proofs.ApplyReconcile.RuntimeBridge

/-!
# Apply/Reconcile Convergence

T-Conv results connecting a complete apply pass to runtime reconcile publication.
-/

namespace ApplyReconcile

/-- Meaningful apply-to-runtime convergence result.

After apply, every behavior id declared by `M` is runnable in the resolved
snapshot. This uses `apply_realizes_manifest` non-trivially (via `hM`, `hL`) to
witness each behavior document in the post-apply live state. -/
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

/-- **T-Conv — coverage/corollary form (ResolvedSnapshot form).**

    This theorem is the structural coverage statement for
    `toResolvedSnapshot`: `runnable ∪ unavailable` equals the carrier set
    supplied to the bridge. The proof does not use `hM`/`hL`; the stronger
    apply-sensitive fact is `t_conv_runnable`, and `t_conv_no_unavailable`
    composes that fact to show the unavailable set is empty after a
    well-formed apply. -/
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

/-- **T-Conv — published coverage form.** Corollary of t_conv: once the resolved
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

end ApplyReconcile
