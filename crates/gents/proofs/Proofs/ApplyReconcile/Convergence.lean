import Proofs.ApplyReconcile.RuntimeBridge

namespace ApplyReconcile

theorem t_conv_runnable
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (defaultBehavior : BehaviorId) :
    let L' := applyAll L (diff M L)
    (L'.toResolvedSnapshot defaultBehavior M.behaviorIds).runnable = M.behaviorIds := by
  simp only
  unfold LiveState.toResolvedSnapshot
  simp only
  apply Finset.ext
  intro bid
  constructor
  ·
    intro h
    exact (@Finset.filter_subset _ _ (Classical.decPred _) M.behaviorIds) h
  ·
    intro hbid
    rw [@Finset.mem_filter _ _ (Classical.decPred _)]
    refine ⟨hbid, ?_⟩
    unfold Manifest.behaviorIds at hbid
    rw [Finset.mem_image] at hbid
    obtain ⟨d, hd_in_filter, hd_id⟩ := hbid
    rw [Finset.mem_filter] at hd_in_filter
    obtain ⟨hd_support, hd_coll⟩ := hd_in_filter
    have hd_some : (M.docs d).isSome = true := (M.support_iff d).mp hd_support
    obtain ⟨f, hf⟩ := Option.isSome_iff_exists.mp hd_some
    have hL'd : (applyAll L (diff M L)).desired d = some f :=
      apply_realizes_manifest hM hL d f hf
    have hcontains : (applyAll L (diff M L)).contains d = true := by
      unfold LiveState.contains
      rw [hL'd]; rfl
    refine ⟨d, ?_, hcontains⟩
    unfold DocRef.behaviorId?
    rw [hd_coll]
    exact congrArg some hd_id

theorem t_conv
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed) (_hL : L.WellFormed)
    (defaultBehavior : BehaviorId) :
    let L' := applyAll L (diff M L)
    let snapshot := L'.toResolvedSnapshot defaultBehavior M.behaviorIds
    snapshot.runnable ∪ snapshot.unavailable = M.behaviorIds := by
  exact LiveState.toResolvedSnapshot_coverage _ _ _

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
  have h := t_conv_runnable hM hL defaultBehavior
  unfold LiveState.toResolvedSnapshot at h
  simp only at h
  rw [h]
  exact Finset.sdiff_self _

theorem t_conv_published
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (defaultBehavior : BehaviorId)
    (gen : Generation) :
    let L' := applyAll L (diff M L)
    let snapshot := L'.toResolvedSnapshot defaultBehavior M.behaviorIds
    let active := snapshot.activate gen
    active.runnable ∪ active.unavailable = M.behaviorIds := by
  simpa [ResolvedSnapshot.activate] using t_conv hM hL defaultBehavior

end ApplyReconcile
