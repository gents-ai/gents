import Proofs.Retrieval.Types

namespace Retrieval

theorem le_maxOf {s : Nat} : ∀ {l : List Nat}, s ∈ l → s ≤ maxOf l
  | [], h => by cases h
  | a :: rest, h => by
    simp only [List.mem_cons] at h
    simp only [maxOf]
    rcases h with rfl | h
    · exact Nat.le_max_left _ _
    · exact Nat.le_trans (le_maxOf h) (Nat.le_max_right _ _)

theorem advanceFuel_spec (covered : List Nat) :
    ∀ fuel w,
      w ≤ advanceFuel covered fuel w ∧
        (∀ s, w ≤ s → s < advanceFuel covered fuel w → s ∈ covered) ∧
        (advanceFuel covered fuel w ∉ covered ∨ advanceFuel covered fuel w = w + fuel)
  | 0, w => by
    refine ⟨Nat.le_refl _, fun s h₁ h₂ => ?_, Or.inr rfl⟩
    simp only [advanceFuel] at h₂
    omega
  | fuel + 1, w => by
    by_cases h : w ∈ covered
    · obtain ⟨ih₁, ih₂, ih₃⟩ := advanceFuel_spec covered fuel (w + 1)
      rw [advanceFuel, if_pos h]
      refine ⟨by omega, fun s h₁ h₂ => ?_, ?_⟩
      · by_cases hs : s = w
        · exact hs ▸ h
        · exact ih₂ s (by omega) h₂
      · rcases ih₃ with ih₃ | ih₃
        · exact Or.inl ih₃
        · exact Or.inr (by omega)
    · rw [advanceFuel, if_neg h]
      exact ⟨Nat.le_refl _, fun s h₁ h₂ => absurd h₂ (by omega), Or.inl h⟩

theorem advance_le (covered : List Nat) (w : Nat) : w ≤ advance covered w :=
  (advanceFuel_spec covered _ w).1

theorem advance_sound (covered : List Nat) (w s : Nat)
    (h₁ : w ≤ s) (h₂ : s < advance covered w) : s ∈ covered :=
  (advanceFuel_spec covered _ w).2.1 s h₁ h₂

/-- The watermark always rests on an uncovered sequence. -/
theorem advance_not_covered (covered : List Nat) (w : Nat) :
    advance covered w ∉ covered := by
  rcases (advanceFuel_spec covered (maxOf covered + 1) w).2.2 with h | h
  · exact h
  · intro hmem
    have := le_maxOf hmem
    simp only [advance] at this
    omega

theorem step_watermark_monotone (msgs : List Message) (st : SessionIndex) :
    st.watermark ≤ (step msgs st).next.watermark :=
  advance_le _ _

/-- Every sequence the watermark passes owns chunks. -/
theorem step_watermark_sound (msgs : List Message) (st : SessionIndex) (s : Nat)
    (h₁ : st.watermark ≤ s) (h₂ : s < (step msgs st).next.watermark) :
    s ∈ (step msgs st).next.indexed :=
  advance_sound _ _ _ h₁ h₂

/-- One pass indexes every settled message the scan reaches. -/
theorem step_indexes_settled (msgs : List Message) (st : SessionIndex)
    (m : Message) (hm : m ∈ msgs) (hs : m.settled = true)
    (hw : st.watermark ≤ m.sequence) :
    m.sequence ∈ (step msgs st).next.indexed := by
  simp only [step, List.mem_filter, List.mem_append, decide_eq_true_eq]
  refine ⟨?_, List.mem_map_of_mem _ hm⟩
  by_cases hi : m.sequence ∈ st.indexed
  · exact Or.inl hi
  · refine Or.inr (List.mem_map.mpr ⟨m, ?_, rfl⟩)
    simp only [List.mem_filter, hs, hw, hi, decide_true, decide_false, Bool.and_self,
      Bool.not_false, and_true, not_false_eq_true]
    exact hm

/-- Unsettled responses are never embedded. -/
theorem step_indexes_only_settled (msgs : List Message) (st : SessionIndex)
    (s : Nat) (h : s ∈ (step msgs st).index) :
    ∃ m ∈ msgs, m.sequence = s ∧ m.settled = true := by
  simp only [step, toIndex, List.mem_map, List.mem_filter, Bool.and_eq_true] at h
  obtain ⟨m, ⟨hm, ⟨hs, _⟩, _⟩, rfl⟩ := h
  exact ⟨m, hm, rfl, hs⟩

/-- Chunks never outlive their canonical message. -/
theorem step_indexed_present (msgs : List Message) (st : SessionIndex) (s : Nat)
    (h : s ∈ (step msgs st).next.indexed) : s ∈ present msgs := by
  simp only [step, List.mem_filter, decide_eq_true_eq] at h
  exact h.2

/-- A second pass over unchanged messages does no work. -/
theorem step_second_pass_idle (msgs : List Message) (st : SessionIndex) :
    (step msgs (step msgs st).next).index = [] ∧
      (step msgs (step msgs st).next).delete = [] := by
  constructor
  · simp only [step, toIndex]
    rw [List.map_eq_nil_iff, List.filter_eq_nil_iff]
    intro m hm hpred
    simp only [Bool.and_eq_true, decide_eq_true_eq] at hpred
    obtain ⟨⟨hs, hw⟩, hni⟩ := hpred
    exact hni (step_indexes_settled msgs st m hm hs
      (Nat.le_trans (step_watermark_monotone msgs st) hw))
  · simp only [step, toDelete]
    rw [List.filter_eq_nil_iff]
    intro s hs hpred
    simp only [decide_eq_true_eq] at hpred
    exact hpred (step_indexed_present msgs st s hs)

/-- A crash after the chunk writes and before the watermark write replays to
the same durable state, with no further chunk work. -/
theorem crash_replay_converges (msgs : List Message) (st : SessionIndex) :
    step msgs (crashedAfterChunks msgs st) =
      { next := (step msgs st).next, index := [], delete := [] } := by
  have hidx : toIndex msgs (crashedAfterChunks msgs st) = [] := by
    simp only [toIndex, crashedAfterChunks]
    rw [List.map_eq_nil_iff, List.filter_eq_nil_iff]
    intro m hm hpred
    simp only [Bool.and_eq_true, decide_eq_true_eq] at hpred
    obtain ⟨⟨hs, hw⟩, hni⟩ := hpred
    exact hni (step_indexes_settled msgs st m hm hs hw)
  have hdel : toDelete msgs (crashedAfterChunks msgs st) = [] := by
    simp only [toDelete, crashedAfterChunks]
    rw [List.filter_eq_nil_iff]
    intro s hs hpred
    simp only [decide_eq_true_eq] at hpred
    exact hpred (step_indexed_present msgs st s hs)
  have hcov :
      ((crashedAfterChunks msgs st).indexed ++ []).filter
          (fun s => decide (s ∈ present msgs)) =
        (step msgs st).next.indexed := by
    rw [List.append_nil]
    show List.filter _ (step msgs st).next.indexed = _
    rw [List.filter_eq_self]
    intro s hs
    simpa using step_indexed_present msgs st s hs
  have hw : (crashedAfterChunks msgs st).watermark = st.watermark := rfl
  simp only [step] at hcov ⊢
  rw [hidx, hdel, hcov]
  simp only [crashedAfterChunks, step]

/-- A fresh session has chunks for every present message. -/
theorem fresh_covers_every_message (msgs : List Message) (st : SessionIndex)
    (h : pending msgs st = 0) (m : Message) (hm : m ∈ msgs) :
    m.sequence ∈ st.indexed := by
  simp only [pending, List.length_eq_zero_iff, List.filter_eq_nil_iff,
    decide_eq_true_eq] at h
  exact Classical.not_not.mp (h m hm)

/-- Once every message is settled, one pass leaves the session fresh. -/
theorem settled_session_fresh_after_one_pass (msgs : List Message) (st : SessionIndex)
    (h : ∀ m ∈ msgs, m.settled = true ∧ (st.watermark ≤ m.sequence ∨ m.sequence ∈ st.indexed)) :
    pending msgs (step msgs st).next = 0 := by
  simp only [pending, List.length_eq_zero_iff, List.filter_eq_nil_iff, decide_eq_true_eq,
    Classical.not_not]
  intro m hm
  obtain ⟨hs, hw | hi⟩ := h m hm
  · exact step_indexes_settled msgs st m hm hs hw
  · simp only [step, List.mem_filter, List.mem_append, decide_eq_true_eq]
    exact ⟨Or.inl hi, List.mem_map_of_mem _ hm⟩

/-- Semantic hits come only from candidates whose source materializes. -/
theorem selectHits_authorized (cands : List Candidate) (authorized : List Nat)
    (limit : Nat) (c : Candidate) (h : c ∈ selectHits cands authorized limit) :
    c ∈ cands ∧ c.ref ∈ authorized := by
  have hm := List.mem_of_mem_take h
  rw [(List.mergeSort_perm _ _).mem_iff, List.mem_filter, decide_eq_true_eq] at hm
  exact hm

theorem selectHits_bounded (cands : List Candidate) (authorized : List Nat) (limit : Nat) :
    (selectHits cands authorized limit).length ≤ limit :=
  List.length_take_le _ _

theorem ranksBefore_trans (a b c : Candidate)
    (h₁ : ranksBefore a b = true) (h₂ : ranksBefore b c = true) :
    ranksBefore a c = true := by
  simp only [ranksBefore, Bool.or_eq_true, decide_eq_true_eq, Bool.and_eq_true, beq_iff_eq]
    at h₁ h₂ ⊢
  omega

theorem ranksBefore_total (a b : Candidate) :
    (ranksBefore a b || ranksBefore b a) = true := by
  simp only [ranksBefore, Bool.or_eq_true, decide_eq_true_eq, Bool.and_eq_true, beq_iff_eq]
  omega

/-- Hits are nearest first. -/
theorem selectHits_ordered (cands : List Candidate) (authorized : List Nat) (limit : Nat) :
    (selectHits cands authorized limit).Pairwise (fun a b => ranksBefore a b = true) :=
  (List.sorted_mergeSort ranksBefore_trans ranksBefore_total _).sublist
    (List.take_sublist _ _)

end Retrieval
