import Proofs.ApplyReconcile.Apply

namespace ApplyReconcile

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

lemma applyAll_append (L : LiveState) (pref : List ApplyStep) (s : ApplyStep) :
    applyAll L (pref ++ [s]) = applyOne (applyAll L pref) s := by
  unfold applyAll
  simp [List.foldl_append]

lemma applyOne_desired_some_of_some (L : LiveState) (s : ApplyStep) (d : DocRef)
    (hwrite : s.payload?.isSome = true)
    (h : (L.desired d).isSome = true) : ((applyOne L s).desired d).isSome = true := by
  unfold applyOne
  by_cases heq : d = s.target
  · simp [heq, hwrite]
  · simp [heq, h]

lemma applyAll_desired_some_of_some (L : LiveState) (steps : List ApplyStep) (d : DocRef)
    (hsteps : ∀ s ∈ steps, s.payload?.isSome = true)
    (h : (L.desired d).isSome = true) : ((applyAll L steps).desired d).isSome = true := by
  induction steps generalizing L with
  | nil => exact h
  | cons s rest ih =>
      show ((applyAll (applyOne L s) rest).desired d).isSome = true
      have hs : s.payload?.isSome = true := hsteps s (List.mem_cons_self _ _)
      have hrest : ∀ s' ∈ rest, s'.payload?.isSome = true :=
        fun s' hmem => hsteps s' (List.mem_cons_of_mem _ hmem)
      exact ih (L := applyOne L s) hrest (applyOne_desired_some_of_some L s d hs h)

lemma applyOne_target_isSome (L : LiveState) (s : ApplyStep) :
    s.payload?.isSome = true →
    ((applyOne L s).desired s.target).isSome = true := by
  intro hwrite
  unfold applyOne; simp [hwrite]

lemma applyAll_desired_some_of_target_mem
    (L : LiveState) (steps : List ApplyStep) (d : DocRef)
    (hsteps : ∀ s ∈ steps, s.payload?.isSome = true)
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
      ·
        have hs_write : s.payload?.isSome = true := hsteps s (List.mem_cons_self _ _)
        subst heq
        have hrest : ∀ s' ∈ rest, s'.payload?.isSome = true :=
          fun s' hmem => hsteps s' (List.mem_cons_of_mem _ hmem)
        apply applyAll_desired_some_of_some
        · exact hrest
        subst htgt'
        exact applyOne_target_isSome _ _ hs_write
      ·
        have hrest : ∀ s' ∈ rest, s'.payload?.isSome = true :=
          fun s' hmem => hsteps s' (List.mem_cons_of_mem _ hmem)
        exact ih (L := applyOne L s) hrest ⟨s', hmem_rest, htgt'⟩

lemma mem_prefix_of_lower_rank
    {M : Manifest} {L : LiveState}
    {pref : List ApplyStep} {s s' : ApplyStep}
    (hpref : List.IsPrefix (pref ++ [s]) (diff M L))
    (hmem' : s' ∈ diff M L)
    (hlt : s'.target.collection.applyOrder < s.target.collection.applyOrder) :
    s' ∈ pref := by
  obtain ⟨suf, hsuf⟩ := hpref
  rw [← hsuf] at hmem'
  obtain ⟨k', hk'_lt, hk'_eq⟩ := List.getElem_of_mem hmem'
  by_cases hcase : k' < pref.length
  ·
    have hk'_in_left : k' < (pref ++ [s]).length := by simp; omega
    have hk'_in_pref : k' < pref.length := hcase
    have h_in : s' ∈ pref := by
      rw [← hk'_eq]
      rw [List.getElem_append_left hk'_in_left]
      rw [List.getElem_append_left hk'_in_pref]
      exact List.getElem_mem _
    exact h_in
  · push_neg at hcase
    have hpref_lt_len : pref.length < (pref ++ [s] ++ suf).length := by
      simp
    have hpref_lt_diff : pref.length < (diff M L).length := by
      rw [← hsuf]; exact hpref_lt_len
    have hk'_lt_diff : k' < (diff M L).length := by
      rw [← hsuf]; exact hk'_lt
    have hsort := diff_sorted_by_applyOrder M L pref.length k' hcase hk'_lt_diff
    have hpref_at_s :
        ((diff M L).get ⟨pref.length, hpref_lt_diff⟩).target.collection.applyOrder
          = s.target.collection.applyOrder := by
      simp only [List.get_eq_getElem]
      have h1 : (pref ++ [s] ++ suf)[pref.length]'hpref_lt_len = s := by
        rw [List.getElem_append_left (by simp)]
        rw [List.getElem_append_right (by simp)]
        simp
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

lemma apply_preserves_wellFormed
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed) :
    ∀ pref : List ApplyStep,
      List.IsPrefix pref (diff M L) →
      (applyAll L pref).WellFormed := by
  intro pref
  induction pref using List.reverseRecOn with
  | nil =>
      intro _hpref
      show (applyAll L []).WellFormed
      change L.WellFormed
      exact hL
  | append_singleton pref' s ih =>
      intro hpref
      have hpref' : List.IsPrefix pref' (diff M L) := by
        obtain ⟨suf, hsuf⟩ := hpref
        refine ⟨[s] ++ suf, ?_⟩
        rw [← hsuf]; simp [List.append_assoc]
      have ih_wf := ih hpref'
      have hs_mem : s ∈ diff M L := by
        obtain ⟨suf, hsuf⟩ := hpref
        rw [← hsuf]
        simp
      have hs_payload_some : s.payload? = some s.payload :=
        diff_step_payload_eq_some hs_mem
      have hs_write : s.payload?.isSome = true :=
        diff_step_payload_isSome hs_mem
      have hpref'_mem : ∀ t ∈ pref', t ∈ diff M L := by
        intro t ht
        obtain ⟨suf, hsuf⟩ := hpref'
        rw [← hsuf]
        exact List.mem_append_left _ ht
      have hpref'_write : ∀ t ∈ pref', t.payload?.isSome = true :=
        fun t ht => diff_step_payload_isSome (hpref'_mem t ht)
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
      have happ : applyAll L (pref' ++ [s]) = applyOne (applyAll L pref') s :=
        applyAll_append L pref' s
      refine ⟨?_, ?_⟩
      ·
        intro d f hf r hr
        rw [happ] at hf
        by_cases heq : d = s.target
        ·
          have hf_payload : s.payload = f := by
            have : (applyOne (applyAll L pref') s).desired d = some s.payload := by
              unfold applyOne; simp [heq, hs_payload_some]
            rw [this] at hf
            exact Option.some.inj hf
          rw [← hf_payload] at hr
          have hr_in_M : M.contains r = true := hM.1 s.target s.payload hMd r hr
          have hr_rank : r.collection.applyOrder < s.target.collection.applyOrder :=
            hM.2 s.target s.payload hMd r hr
          show ((applyAll L (pref' ++ [s])).desired r).isSome = true
          rw [happ]
          apply applyOne_desired_some_of_some
          · exact hs_write
          cases hLd : L.desired r with
          | some _ =>
              apply applyAll_desired_some_of_some
              · exact hpref'_write
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
                  · exact hpref'_write
                  exact ⟨s_r, hs_r_in_pref', hs_r_tgt⟩
        ·
          have hpre : (applyOne (applyAll L pref') s).desired d = (applyAll L pref').desired d :=
            applyOne_desired_ne _ _ _ heq
          rw [hpre] at hf
          have hrclo := ih_wf.1 d f hf r hr
          show ((applyAll L (pref' ++ [s])).desired r).isSome = true
          rw [happ]
          exact applyOne_desired_some_of_some _ _ _ hs_write hrclo
      ·
        intro d f hf r hr
        rw [happ] at hf
        by_cases heq : d = s.target
        · have hf_payload : s.payload = f := by
            have : (applyOne (applyAll L pref') s).desired d = some s.payload := by
              unfold applyOne; simp [heq, hs_payload_some]
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

theorem apply_realizes_desired
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f, M.docs d = some f →
      (applyAll L (diff M L)).desired d = some f :=
  apply_realizes_manifest hM hL

end ApplyReconcile
