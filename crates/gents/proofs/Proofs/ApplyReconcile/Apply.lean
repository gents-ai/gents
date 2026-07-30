import Proofs.ApplyReconcile.Diff

namespace ApplyReconcile

def applyOne (L : LiveState) (s : ApplyStep) : LiveState where
  desired := fun d => if d = s.target then s.payload? else L.desired d
  live    := L.live

def applyAll (L : LiveState) (steps : List ApplyStep) : LiveState :=
  steps.foldl applyOne L

lemma applyOne_desired_ne
    (L : LiveState) (s : ApplyStep) (d : DocRef) (h : d ≠ s.target) :
    (applyOne L s).desired d = L.desired d := by
  unfold applyOne
  simp [h]

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

lemma applyAll_desired_of_unique_target
    (L : LiveState) (steps : List ApplyStep) (s : ApplyStep) (d : DocRef)
    (hpayload : s.payload? = some s.payload)
    (hmem : s ∈ steps)
    (htgt : s.target = d)
    (hunique : ∀ s' ∈ steps, s'.target = d → s' = s) :
    (applyAll L steps).desired d = some s.payload := by
  induction steps generalizing L with
  | nil => exact absurd hmem (List.not_mem_nil _)
  | cons head rest ih =>
      show (applyAll (applyOne L head) rest).desired d = some s.payload
      rcases List.mem_cons.mp hmem with heq | hmem_rest
      ·
        subst heq
        by_cases hin : ∃ s' ∈ rest, s'.target = d
        · rcases hin with ⟨s', hs'mem, hs'tgt⟩
          have : s' = s :=
            hunique s' (List.mem_cons_of_mem _ hs'mem) hs'tgt
          subst this
          have hunique_rest : ∀ s'' ∈ rest, s''.target = d → s'' = s' := by
            intro s'' hmem'' ht''
            exact hunique s'' (List.mem_cons_of_mem _ hmem'') ht''
          exact ih (L := applyOne L s') hs'mem hunique_rest
        · push_neg at hin
          have hno : ∀ s'' ∈ rest, s''.target ≠ d := hin
          rw [applyAll_desired_of_not_mem _ _ _ hno]
          have : (applyOne L s).desired d = some s.payload := by
            unfold applyOne
            simp [htgt, hpayload]
          exact this
      ·
        have hunique_rest : ∀ s' ∈ rest, s'.target = d → s' = s := by
          intro s' hmem' ht'
          exact hunique s' (List.mem_cons_of_mem _ hmem') ht'
        exact ih (L := applyOne L head) hmem_rest hunique_rest

lemma apply_realizes_manifest
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed)
    (_hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f, M.docs d = some f →
      (applyAll L (diff M L)).desired d = some f := by
  intro d f hf
  have hd_support : d ∈ M.support := by
    rw [M.support_iff d]; rw [hf]; rfl
  cases hLd : L.desired d with
  | none =>
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
                simp at h
                have : s' = ApplyStep.create d' f' := h.symm
                subst this
                have hdeq : d' = d := htgt'
                subst hdeq
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
                  have hffeq : f' = f := by
                    have h1 : some f = some f' := hf.symm.trans hMd'
                    exact (Option.some.inj h1).symm
                  subst hffeq
                  rw [hLd] at hLd'
                  exact absurd hLd' (by simp)
      have hs_payload_some : s.payload? = some s.payload := rfl
      have := applyAll_desired_of_unique_target L (diff M L) s d
        hs_payload_some hs_mem hs_tgt hunique
      rw [hs_pay] at this
      exact this
  | some g =>
      by_cases hfg : f = g
      ·
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
                    have hgg : g' = g := by
                      have h1 : some g = some g' := hLd.symm.trans hLd'
                      exact (Option.some.inj h1).symm
                    subst hgg
                    exact hfg' hfg
        rw [applyAll_desired_of_not_mem _ _ _ hno, hLd, hfg]
      ·
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
        have hs_payload_some : s.payload? = some s.payload := rfl
        have := applyAll_desired_of_unique_target L (diff M L) s d
          hs_payload_some hs_mem hs_tgt hunique
        rw [hs_pay] at this
        exact this

end ApplyReconcile
