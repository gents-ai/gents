import Proofs.EditMatch.Model

namespace Proofs.EditMatch

variable (fold : Char → Char)

theorem keyAt_exact_inj {a b : Line} (h : keyAt fold .exact a = keyAt fold .exact b) :
    a = b := by
  cases a; cases b
  simp [keyAt] at h
  simp [h.1, h.2.1, h.2.2]

theorem key_coarsen_trailingWs {a b : Line}
    (h : keyAt fold .exact a = keyAt fold .exact b) :
    keyAt fold .trailingWs a = keyAt fold .trailingWs b := by
  simp [keyAt] at h ⊢
  exact ⟨h.1, h.2.1⟩

theorem key_coarsen_trim {a b : Line}
    (h : keyAt fold .trailingWs a = keyAt fold .trailingWs b) :
    keyAt fold .trim a = keyAt fold .trim b := by
  simp [keyAt] at h ⊢
  exact h.2

theorem key_coarsen_unicode {a b : Line}
    (h : keyAt fold .trim a = keyAt fold .trim b) :
    keyAt fold .unicode a = keyAt fold .unicode b := by
  simp [keyAt] at h ⊢
  exact congrArg (List.map fold) h

theorem occurrences_sound {s : Strategy} {doc pat : Doc} {i : Nat}
    (h : i ∈ occurrences fold s doc pat) :
    matchesAt fold s doc pat i = true :=
  (List.mem_filter.mp h).2

theorem ladderMatch_sound {doc pat : Doc} {s : Strategy} {occ : List Nat}
    (h : ladderMatch fold doc pat = some (s, occ)) :
    ∀ i ∈ occ, matchesAt fold s doc pat i = true := by
  intro i hi
  have hocc : occ = occurrences fold s doc pat := by
    have aux : ∀ ss : List Strategy,
        ladderMatch.go fold doc pat ss = some (s, occ) →
        occ = occurrences fold s doc pat := by
      intro ss
      induction ss with
      | nil => intro h; simp [ladderMatch.go] at h
      | cons head rest ih =>
        intro h
        by_cases hempty : (occurrences fold s doc pat).isEmpty
        all_goals
          simp only [ladderMatch.go] at h
          split at h
          · exact ih h
          · simp at h; simp [← h.1, h.2]
    exact aux Strategy.ladder h
  subst hocc
  exact occurrences_sound fold hi

theorem selectDisjointGo_subset {len : Nat} :
    ∀ (fuel : Nat) (occ : List Nat) (i : Nat),
      i ∈ selectDisjointGo len fuel occ → i ∈ occ := by
  intro fuel
  induction fuel with
  | zero => intro occ i h; simp [selectDisjointGo] at h
  | succ n ih =>
    intro occ i h
    cases occ with
    | nil => simp [selectDisjointGo] at h
    | cons head rest =>
      simp only [selectDisjointGo, List.mem_cons] at h
      rcases h with rfl | h
      · exact List.mem_cons_self ..
      · exact List.mem_cons_of_mem _ (List.mem_filter.mp (ih _ _ h)).1

theorem selectDisjoint_subset {len : Nat} {occ : List Nat} {i : Nat}
    (h : i ∈ selectDisjoint len occ) : i ∈ occ :=
  selectDisjointGo_subset _ _ _ h

theorem selectDisjoint_pairwise_gap {len : Nat} :
    ∀ (fuel : Nat) (occ : List Nat),
      (selectDisjointGo len fuel occ).Pairwise (fun a b => a + len ≤ b) := by
  intro fuel
  induction fuel with
  | zero => intro occ; simp [selectDisjointGo]
  | succ n ih =>
    intro occ
    cases occ with
    | nil => simp [selectDisjointGo]
    | cons head rest =>
      simp only [selectDisjointGo]
      refine List.Pairwise.cons ?_ (ih _)
      intro b hb
      have hmem := selectDisjointGo_subset (len := len) _ _ _ hb
      simpa using (List.mem_filter.mp hmem).2

theorem ladderMatch_exact_priority {doc pat : Doc}
    (h : (occurrences fold .exact doc pat).isEmpty = false) :
    ladderMatch fold doc pat = some (.exact, occurrences fold .exact doc pat) := by
  simp [ladderMatch, ladderMatch.go, Strategy.ladder, h]

theorem stale_rejects {doc expected : Doc} {req : Request}
    (hexp : req.expected = some expected) (hne : expected ≠ doc) :
    decide fold doc req = .rejectedStale := by
  simp [decide, hexp, hne]

theorem stale_never_writes {doc expected : Doc} {req : Request}
    (hexp : req.expected = some expected) (hne : expected ≠ doc) :
    applyFs fold doc req = doc := by
  simp [applyFs, stale_rejects (fold := fold) hexp hne]

theorem ambiguous_needs_multiple {doc : Doc} {req : Request} {s : Strategy}
    {n : Nat} (h : decideMatched fold doc req = .ambiguous s n) :
    2 ≤ n ∧ req.replaceAll = false := by
  unfold decideMatched at h
  split at h
  · exact absurd h (by simp)
  · rename_i sM occM heq
    split at h
    · split at h <;> exact absurd h (by simp)
    · rename_i hcond
      have hlen : (selectDisjoint req.pattern.length occM).length ≠ 1 :=
        fun h1 => hcond (Or.inl h1)
      have hall : req.replaceAll ≠ true := fun ha => hcond (Or.inr ha)
      simp only [Outcome.ambiguous.injEq] at h
      obtain ⟨hs, hn⟩ := h
      subst hn
      refine ⟨?_, Bool.eq_false_iff.mpr hall⟩
      have hne : occM ≠ [] := by
        have aux : ∀ ss : List Strategy,
            ladderMatch.go fold doc req.pattern ss = some (sM, occM) →
            occM ≠ [] := by
          intro ss
          induction ss with
          | nil => intro hgo; simp [ladderMatch.go] at hgo
          | cons head rest ih =>
            intro hgo
            simp only [ladderMatch.go] at hgo
            split at hgo
            · exact ih hgo
            · rename_i hnonempty
              cases hgo
              simpa [List.isEmpty_iff] using hnonempty
        exact aux Strategy.ladder heq
      have hsel : selectDisjoint req.pattern.length occM ≠ [] := by
        cases occM with
        | nil => exact absurd rfl hne
        | cons head rest => simp [selectDisjoint, selectDisjointGo]
      have hpos : 0 < (selectDisjoint req.pattern.length occM).length :=
        List.length_pos_iff.mpr hsel
      omega

theorem apply_writes_decided {doc result : Doc} {req : Request} {s : Strategy}
    (h : decide fold doc req = .applied result s) :
    applyFs fold doc req = result := by
  simp [applyFs, h]

theorem non_applied_never_writes {doc : Doc} {req : Request}
    (h : ∀ result s, decide fold doc req ≠ .applied result s) :
    applyFs fold doc req = doc := by
  unfold applyFs
  split
  · rename_i r s heq; exact absurd heq (h r s)
  all_goals rfl

theorem dry_run_never_writes (doc : Doc) (req : Request) :
    dryRunFs fold doc req = doc := rfl

theorem applied_changes {doc result : Doc} {req : Request} {s : Strategy}
    (h : decideMatched fold doc req = .applied result s) :
    result ≠ doc := by
  unfold decideMatched at h
  split at h
  · exact absurd h (by simp)
  · split at h
    · split at h
      · exact absurd h (by simp)
      · rename_i hne
        simp only [Outcome.applied.injEq] at h
        exact h.1 ▸ hne
    · exact absurd h (by simp)

theorem noop_is_honest {doc : Doc} {req : Request} {s : Strategy}
    {occ : List Nat}
    (hl : ladderMatch fold doc req.pattern = some (s, occ))
    (hg : (selectDisjoint req.pattern.length occ).length = 1
      ∨ req.replaceAll = true)
    (h : decideMatched fold doc req = .noop s) :
    chosenResult s doc req (selectDisjoint req.pattern.length occ) = doc := by
  unfold decideMatched at h
  rw [hl] at h
  simp only [hg, if_true] at h
  split at h
  · assumption
  · exact absurd h (by simp)

theorem windowMatches_exact_eq {window pat : Doc}
    (h : windowMatches fold .exact window pat = true) : window = pat := by
  unfold windowMatches at h
  simp only [Bool.and_eq_true, beq_iff_eq, List.all_eq_true] at h
  obtain ⟨hlen, hall⟩ := h
  apply List.ext_getElem hlen
  intro i h1 h2
  have hzip : i < (window.zip pat).length := by
    simpa [List.length_zip, hlen] using h2
  have hmem : (window[i], pat[i]) ∈ window.zip pat := by
    have hget : (window.zip pat)[i] = (window[i], pat[i]) := by
      simp [List.getElem_zip]
    exact hget ▸ List.getElem_mem hzip
  exact keyAt_exact_inj (fold := fold) (by simpa using hall _ hmem)

theorem matchesAt_exact_window {doc pat : Doc} {i : Nat}
    (h : matchesAt fold .exact doc pat i = true) :
    (doc.drop i).take pat.length = pat :=
  windowMatches_exact_eq (fold := fold) h

end Proofs.EditMatch
