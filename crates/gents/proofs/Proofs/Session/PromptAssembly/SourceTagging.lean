import Proofs.Session.PromptAssembly.ClaudeMap

/-!
# Source-tag issuance for projected assistant occurrences

When provider-view projection or history sanitization maps loaded canonical rows
(`originals`) to a projected sequence (`sourced`), each projected item names its
loaded origin by `sourceIndex` and (for assistant rows) the positions of its
surviving blocks (`blockIndices`).

This module owns the fail-closed decision (`taggedFromSourced`) that validates
the projection against the loaded rows and issues each emitted row's source tag,
physical header, composed canonical block indices, and surviving stub receipts.
Downstream checkpoint and reasoning-suffix models (`ClaudeMap`) consume the
resulting tagged assistant rows.
-/

namespace PromptAssembly.SourceTagging

open PromptAssembly.ClaudeMap

inductive RowRole where
  | user
  | assistant
  | system
  deriving DecidableEq, Repr

structure LoadedRow where
  role : RowRole
  source : Option ReplayTag := none
  physicalHeader : Option String := none
  blockIndices : List Nat := []
  blockCount : Nat := 0
  stubReceiptKeys : List String := []
  deriving DecidableEq, Repr

structure SourcedRow where
  sourceIndex : Nat
  role : RowRole
  blockCount : Nat := 0
  blockIndices : List Nat := []
  keptToolResultKeys : List String := []
  deriving DecidableEq, Repr

structure TaggedRow where
  role : RowRole
  source : Option ReplayTag := none
  physicalHeader : Option String := none
  blockIndices : List Nat := []
  stubReceiptKeys : List String := []
  deriving DecidableEq, Repr

inductive SourceTagError where
  | nonAssistantSource
  | reorderedOrDuplicatedSourceRow
  | outOfRangeSourceIndex
  | roleMismatch
  | misalignedAssistantBlockIndices
  | reorderedAssistantBlocks
  | outOfRangeAssistantBlockIndex
  | invalidOriginalAssociation
  deriving DecidableEq, Repr

def mapBlockIndices (originalIndices : List Nat) : List Nat → Option (List Nat)
  | [] => some []
  | pos :: rest => do
      let orig ← originalIndices[pos]?
      let tail ← mapBlockIndices originalIndices rest
      some (orig :: tail)

def isUnhydrated (row : LoadedRow) : Bool :=
  row.physicalHeader.isNone && row.blockIndices.isEmpty

def originalAssociationValid (row : LoadedRow) : Bool :=
  row.role == .assistant &&
    row.blockIndices.length == row.blockCount &&
    strictlyIncreasingIndices row.blockIndices

def tagOneRow (original : LoadedRow) (item : SourcedRow) : Except SourceTagError TaggedRow :=
  if original.source.isSome && item.role != .assistant then
    .error .nonAssistantSource
  else if original.role != item.role then
    .error .roleMismatch
  else match item.role with
    | .assistant =>
        if item.blockIndices.length != item.blockCount then
          .error .misalignedAssistantBlockIndices
        else if !strictlyIncreasingIndices item.blockIndices then
          .error .reorderedAssistantBlocks
        else if isUnhydrated original then
          if !item.blockIndices.all (· < original.blockCount) then
            .error .outOfRangeAssistantBlockIndex
          else
            .ok {
              role := .assistant
              source := original.source
              physicalHeader := none
              blockIndices := []
              stubReceiptKeys := []
            }
        else
          match mapBlockIndices original.blockIndices item.blockIndices with
          | none => .error .outOfRangeAssistantBlockIndex
          | some composed =>
              if !originalAssociationValid original then
                .error .invalidOriginalAssociation
              else
                .ok {
                  role := .assistant
                  source := original.source
                  physicalHeader := original.physicalHeader
                  blockIndices := composed
                  stubReceiptKeys := []
                }
    | role =>
        .ok {
          role
          source := original.source
          physicalHeader := original.physicalHeader
          blockIndices := []
          stubReceiptKeys :=
            original.stubReceiptKeys.filter fun key => item.keptToolResultKeys.contains key
        }

def taggedFromSourcedLoop (originals : List LoadedRow) :
    Option Nat → List SourcedRow → Except SourceTagError (List TaggedRow)
  | _, [] => .ok []
  | previous, item :: rest =>
      if previous.any (fun prev => decide (item.sourceIndex ≤ prev)) then
        .error .reorderedOrDuplicatedSourceRow
      else
        match originals[item.sourceIndex]? with
        | none => .error .outOfRangeSourceIndex
        | some original =>
            match tagOneRow original item with
            | .error err => .error err
            | .ok tagged =>
                match taggedFromSourcedLoop originals (some item.sourceIndex) rest with
                | .error err => .error err
                | .ok tail => .ok (tagged :: tail)

def taggedFromSourced (originals : List LoadedRow) (sourced : List SourcedRow) :
    Except SourceTagError (List TaggedRow) :=
  if originals.any (fun row => row.source.isSome && row.role != .assistant) then
    .error .nonAssistantSource
  else
    taggedFromSourcedLoop originals none sourced

/-! ## Properties -/

theorem nonAssistant_original_with_source_rejected (originals : List LoadedRow)
    (sourced : List SourcedRow) (row : LoadedRow)
    (hmem : row ∈ originals) (hsrc : row.source.isSome = true) (hrole : row.role ≠ .assistant) :
    taggedFromSourced originals sourced = .error .nonAssistantSource := by
  have hany : originals.any (fun r => r.source.isSome && r.role != .assistant) = true := by
    rw [List.any_eq_true]
    exact ⟨row, hmem, by simp [hsrc, hrole]⟩
  simp [taggedFromSourced, hany]

theorem tagOneRow_preserves_role_and_source (original : LoadedRow) (item : SourcedRow)
    (tagged : TaggedRow) (h : tagOneRow original item = .ok tagged) :
    tagged.role = original.role ∧ tagged.role = item.role ∧ tagged.source = original.source := by
  unfold tagOneRow at h
  split at h
  · simp at h
  · split at h
    · simp at h
    · rename_i hroleEq
      have hEq : original.role = item.role := by
        simpa [bne_iff_ne] using hroleEq
      cases hir : item.role <;> simp only [hir] at h hEq
      · cases h
        exact ⟨hEq.symm, rfl, rfl⟩
      · split at h
        · simp at h
        · split at h
          · simp at h
          · split at h
            · split at h
              · simp at h
              · cases h
                exact ⟨hEq.symm, rfl, rfl⟩
            · cases hmap : mapBlockIndices original.blockIndices item.blockIndices with
              | none => simp [hmap] at h
              | some composed =>
                  simp only [hmap] at h
                  split at h
                  · simp at h
                  · cases h
                    exact ⟨hEq.symm, rfl, rfl⟩
      · cases h
        exact ⟨hEq.symm, rfl, rfl⟩

theorem tagOneRow_source_only_on_assistant (original : LoadedRow) (item : SourcedRow)
    (tagged : TaggedRow) (h : tagOneRow original item = .ok tagged)
    (hsrc : tagged.source.isSome = true) :
    tagged.role = .assistant := by
  obtain ⟨_, hrole, htsrc⟩ := tagOneRow_preserves_role_and_source original item tagged h
  unfold tagOneRow at h
  by_cases hna : (original.source.isSome && item.role != .assistant) = true
  · rw [if_pos hna] at h
    simp at h
  · simp only [Bool.and_eq_true, bne_iff_ne, not_and, not_not] at hna
    rw [htsrc] at hsrc
    rw [hrole]
    exact hna hsrc

private theorem taggedFromSourcedLoop_length (originals : List LoadedRow)
    (prev : Option Nat) (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourcedLoop originals prev sourced = .ok emitted) :
    emitted.length = sourced.length := by
  induction sourced generalizing prev emitted with
  | nil =>
      simp [taggedFromSourcedLoop] at h
      subst h
      rfl
  | cons item rest ih =>
      unfold taggedFromSourcedLoop at h
      split at h
      · simp at h
      · cases horig : originals[item.sourceIndex]? with
        | none => simp [horig] at h
        | some orig =>
            simp only [horig] at h
            cases htag : tagOneRow orig item with
            | error err => simp [htag] at h
            | ok tagged =>
                simp only [htag] at h
                cases htail : taggedFromSourcedLoop originals (some item.sourceIndex) rest with
                | error err => simp [htail] at h
                | ok tail =>
                    simp only [htail, Except.ok.injEq] at h
                    subst h
                    simp [ih _ _ htail]

theorem taggedFromSourced_length (originals : List LoadedRow) (sourced : List SourcedRow)
    (emitted : List TaggedRow) (h : taggedFromSourced originals sourced = .ok emitted) :
    emitted.length = sourced.length := by
  unfold taggedFromSourced at h
  split at h
  · simp at h
  · exact taggedFromSourcedLoop_length originals none sourced emitted h

private theorem taggedFromSourcedLoop_gt_prev (originals : List LoadedRow)
    (p : Nat) (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourcedLoop originals (some p) sourced = .ok emitted) :
    ∀ item ∈ sourced, p < item.sourceIndex := by
  induction sourced generalizing p emitted with
  | nil =>
      intro item hmem
      simp at hmem
  | cons item rest ih =>
      unfold taggedFromSourcedLoop at h
      split at h
      · simp at h
      · rename_i hnot
        have hlt : p < item.sourceIndex := by
          simp only [Option.any_some, decide_eq_true_eq, Nat.not_le] at hnot
          exact hnot
        cases horig : originals[item.sourceIndex]? with
        | none => simp [horig] at h
        | some orig =>
            simp only [horig] at h
            cases htag : tagOneRow orig item with
            | error err => simp [htag] at h
            | ok tagged =>
                simp only [htag] at h
                cases htail : taggedFromSourcedLoop originals (some item.sourceIndex) rest with
                | error err => simp [htail] at h
                | ok tail =>
                    intro x hx
                    rcases List.mem_cons.mp hx with rfl | hrest
                    · exact hlt
                    · exact Nat.lt_trans hlt (ih item.sourceIndex tail htail x hrest)

private theorem taggedFromSourcedLoop_pairwise (originals : List LoadedRow)
    (prev : Option Nat) (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourcedLoop originals prev sourced = .ok emitted) :
    List.Pairwise (· < ·) (sourced.map SourcedRow.sourceIndex) := by
  induction sourced generalizing prev emitted with
  | nil => simp
  | cons item rest ih =>
      unfold taggedFromSourcedLoop at h
      split at h
      · simp at h
      · cases horig : originals[item.sourceIndex]? with
        | none => simp [horig] at h
        | some orig =>
            simp only [horig] at h
            cases htag : tagOneRow orig item with
            | error err => simp [htag] at h
            | ok tagged =>
                simp only [htag] at h
                cases htail : taggedFromSourcedLoop originals (some item.sourceIndex) rest with
                | error err => simp [htail] at h
                | ok tail =>
                    simp only [List.map_cons, List.pairwise_cons]
                    refine ⟨?_, ih _ _ htail⟩
                    intro idx hidx
                    rcases List.mem_map.mp hidx with ⟨other, hother, rfl⟩
                    exact taggedFromSourcedLoop_gt_prev originals item.sourceIndex rest tail
                      htail other hother

theorem taggedFromSourced_sourceIndices_strictly_increasing (originals : List LoadedRow)
    (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourced originals sourced = .ok emitted) :
    List.Pairwise (· < ·) (sourced.map SourcedRow.sourceIndex) := by
  unfold taggedFromSourced at h
  split at h
  · simp at h
  · exact taggedFromSourcedLoop_pairwise originals none sourced emitted h

private theorem taggedFromSourcedLoop_in_bounds (originals : List LoadedRow)
    (prev : Option Nat) (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourcedLoop originals prev sourced = .ok emitted) :
    ∀ item ∈ sourced, item.sourceIndex < originals.length := by
  induction sourced generalizing prev emitted with
  | nil =>
      intro item hmem
      simp at hmem
  | cons item rest ih =>
      unfold taggedFromSourcedLoop at h
      split at h
      · simp at h
      · cases horig : originals[item.sourceIndex]? with
        | none => simp [horig] at h
        | some orig =>
            have hlt : item.sourceIndex < originals.length := by
              by_contra hge
              have : originals[item.sourceIndex]? = none :=
                List.getElem?_eq_none (Nat.le_of_not_lt hge)
              rw [this] at horig
              cases horig
            simp only [horig] at h
            cases htag : tagOneRow orig item with
            | error err => simp [htag] at h
            | ok tagged =>
                simp only [htag] at h
                cases htail : taggedFromSourcedLoop originals (some item.sourceIndex) rest with
                | error err => simp [htail] at h
                | ok tail =>
                    intro x hx
                    rcases List.mem_cons.mp hx with rfl | hrest
                    · exact hlt
                    · exact ih _ _ htail x hrest

theorem taggedFromSourced_sourceIndices_in_bounds (originals : List LoadedRow)
    (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourced originals sourced = .ok emitted) :
    ∀ item ∈ sourced, item.sourceIndex < originals.length := by
  unfold taggedFromSourced at h
  split at h
  · simp at h
  · exact taggedFromSourcedLoop_in_bounds originals none sourced emitted h

private theorem taggedFromSourcedLoop_zip_spec (originals : List LoadedRow)
    (prev : Option Nat) (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourcedLoop originals prev sourced = .ok emitted) :
    ∀ pair ∈ sourced.zip emitted,
      ∃ orig, originals[pair.1.sourceIndex]? = some orig ∧
        pair.2.role = orig.role ∧ pair.2.role = pair.1.role ∧
        pair.2.source = orig.source := by
  induction sourced generalizing prev emitted with
  | nil =>
      intro pair hmem
      simp at hmem
  | cons item rest ih =>
      unfold taggedFromSourcedLoop at h
      split at h
      · simp at h
      · cases horig : originals[item.sourceIndex]? with
        | none => simp [horig] at h
        | some orig =>
            simp only [horig] at h
            cases htag : tagOneRow orig item with
            | error err => simp [htag] at h
            | ok tagged =>
                simp only [htag] at h
                cases htail : taggedFromSourcedLoop originals (some item.sourceIndex) rest with
                | error err => simp [htail] at h
                | ok tail =>
                    simp only [htail, Except.ok.injEq] at h
                    subst h
                    intro pair hmem
                    simp only [List.zip_cons_cons, List.mem_cons] at hmem
                    rcases hmem with rfl | hrest
                    · obtain ⟨hr1, hr2, hs⟩ :=
                        tagOneRow_preserves_role_and_source orig item tagged htag
                      exact ⟨orig, horig, hr1, hr2, hs⟩
                    · exact ih _ _ htail pair hrest

theorem taggedFromSourced_preserves_indexed_source_and_role (originals : List LoadedRow)
    (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourced originals sourced = .ok emitted) :
    ∀ pair ∈ sourced.zip emitted,
      ∃ orig, originals[pair.1.sourceIndex]? = some orig ∧
        pair.2.role = orig.role ∧ pair.2.role = pair.1.role ∧
        pair.2.source = orig.source := by
  unfold taggedFromSourced at h
  split at h
  · simp at h
  · exact taggedFromSourcedLoop_zip_spec originals none sourced emitted h

private theorem taggedFromSourcedLoop_source_only_on_assistant (originals : List LoadedRow)
    (prev : Option Nat) (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourcedLoop originals prev sourced = .ok emitted) :
    ∀ row ∈ emitted, row.source.isSome = true → row.role = .assistant := by
  induction sourced generalizing prev emitted with
  | nil =>
      simp [taggedFromSourcedLoop] at h
      subst h
      intro row hmem
      simp at hmem
  | cons item rest ih =>
      unfold taggedFromSourcedLoop at h
      split at h
      · simp at h
      · cases horig : originals[item.sourceIndex]? with
        | none => simp [horig] at h
        | some orig =>
            simp only [horig] at h
            cases htag : tagOneRow orig item with
            | error err => simp [htag] at h
            | ok tagged =>
                simp only [htag] at h
                cases htail : taggedFromSourcedLoop originals (some item.sourceIndex) rest with
                | error err => simp [htail] at h
                | ok tail =>
                    simp only [htail, Except.ok.injEq] at h
                    subst h
                    intro row hmem hsrc
                    rcases List.mem_cons.mp hmem with rfl | hrest
                    · exact tagOneRow_source_only_on_assistant orig item row htag hsrc
                    · exact ih _ _ htail row hrest hsrc

theorem taggedFromSourced_source_only_on_assistant (originals : List LoadedRow)
    (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourced originals sourced = .ok emitted) :
    ∀ row ∈ emitted, row.source.isSome = true → row.role = .assistant := by
  unfold taggedFromSourced at h
  split at h
  · simp at h
  · exact taggedFromSourcedLoop_source_only_on_assistant originals none sourced emitted h

/-! ## Sublist and distinct-tag preservation -/

private theorem sublist_of_indexed_strictly_increasing {α : Type} :
    ∀ (xs : List α) (indices : List Nat) (ys : List α),
      List.Pairwise (· < ·) indices →
      (∀ pair ∈ indices.zip ys, xs[pair.1]? = some pair.2) →
      indices.length = ys.length →
      List.Sublist ys xs := by
  intro xs
  induction xs with
  | nil =>
      intro indices ys _ hlookup hlen
      cases indices with
      | nil =>
          cases ys with
          | nil => exact List.Sublist.slnil
          | cons _ _ => simp at hlen
      | cons idx _ =>
          cases ys with
          | nil => simp at hlen
          | cons y _ =>
              have := hlookup (idx, y) (by simp)
              simp at this
  | cons x rest ih =>
      intro indices ys hpair hlookup hlen
      cases indices with
      | nil =>
          cases ys with
          | nil => exact List.nil_sublist _
          | cons _ _ => simp at hlen
      | cons idx more =>
          cases ys with
          | nil => simp at hlen
          | cons y yrest =>
              have hhead : (x :: rest)[idx]? = some y := hlookup (idx, y) (by simp)
              cases idx with
              | zero =>
                  simp only [List.getElem?_cons_zero, Option.some.injEq] at hhead
                  subst hhead
                  have hgt0 : ∀ k ∈ more, 0 < k := (List.pairwise_cons.mp hpair).1
                  have hmorePair : List.Pairwise (· < ·) more := (List.pairwise_cons.mp hpair).2
                  have hdecPair : List.Pairwise (· < ·) (more.map (· - 1)) := by
                    rw [List.pairwise_map]
                    exact hmorePair.imp_of_mem fun {a} {b} ha hb hab => by
                      have ha0 := hgt0 a ha
                      have hb0 := hgt0 b hb
                      omega
                  have hsub : List.Sublist yrest rest := by
                    apply ih (more.map (· - 1)) yrest hdecPair
                    · intro pair hpairMem
                      have hmemZip : ∃ k ∈ more, k - 1 = pair.1 ∧ (k, pair.2) ∈ more.zip yrest := by
                        rw [List.zip_map_left] at hpairMem
                        rcases List.mem_map.mp hpairMem with ⟨⟨k, v⟩, hkv, heq⟩
                        cases heq
                        exact ⟨k, (List.of_mem_zip hkv).1, rfl, hkv⟩
                      rcases hmemZip with ⟨k, hk, hpair1, hkv⟩
                      have hk0 : 0 < k := hgt0 k hk
                      have hkLookup : (x :: rest)[k]? = some pair.2 :=
                        hlookup (k, pair.2) (by simp [hkv])
                      rw [← hpair1]
                      cases k with
                      | zero => omega
                      | succ k' => simpa using hkLookup
                    · simpa using hlen
                  exact List.Sublist.cons₂ x hsub
              | succ idx' =>
                  have hgt0 : ∀ k ∈ (idx' + 1) :: more, 0 < k := by
                    intro k hk
                    rcases List.mem_cons.mp hk with rfl | hrest
                    · omega
                    · have := (List.pairwise_cons.mp hpair).1 k hrest
                      omega
                  have hdecPair : List.Pairwise (· < ·) (((idx' + 1) :: more).map (· - 1)) := by
                    rw [List.pairwise_map]
                    exact hpair.imp_of_mem fun {a} {b} ha hb hab => by
                      have ha0 := hgt0 a ha
                      have hb0 := hgt0 b hb
                      omega
                  have hsub : List.Sublist (y :: yrest) rest := by
                    apply ih (((idx' + 1) :: more).map (· - 1)) (y :: yrest) hdecPair
                    · intro pair hpairMem
                      rw [List.zip_map_left] at hpairMem
                      rcases List.mem_map.mp hpairMem with ⟨⟨k, v⟩, hkv, heq⟩
                      cases heq
                      have hk0 : 0 < k := hgt0 k (List.of_mem_zip hkv).1
                      have hkLookup : (x :: rest)[k]? = some v := hlookup (k, v) hkv
                      cases k with
                      | zero => omega
                      | succ k' => simpa using hkLookup
                    · simpa using hlen
                  exact List.Sublist.cons x hsub

theorem taggedFromSourced_emitted_sources_sublist (originals : List LoadedRow)
    (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourced originals sourced = .ok emitted) :
    List.Sublist (emitted.filterMap TaggedRow.source) (originals.filterMap LoadedRow.source) := by
  have hlen : sourced.length = emitted.length := (taggedFromSourced_length originals sourced emitted h).symm
  have hpair := taggedFromSourced_sourceIndices_strictly_increasing originals sourced emitted h
  have hzip := taggedFromSourced_preserves_indexed_source_and_role originals sourced emitted h
  have hoptSub : List.Sublist (emitted.map TaggedRow.source) (originals.map LoadedRow.source) := by
    apply sublist_of_indexed_strictly_increasing (originals.map LoadedRow.source)
      (sourced.map SourcedRow.sourceIndex) (emitted.map TaggedRow.source) hpair
    · intro pair hmem
      rw [List.zip_map] at hmem
      rcases List.mem_map.mp hmem with ⟨⟨item, row⟩, hir, heq⟩
      cases heq
      obtain ⟨orig, horig, _, _, hsrc⟩ := hzip (item, row) hir
      simp [List.getElem?_map, horig, hsrc.symm]
    · simp [hlen]
  have hsub : List.Sublist ((emitted.map TaggedRow.source).filterMap id)
      ((originals.map LoadedRow.source).filterMap id) :=
    List.Sublist.filterMap id hoptSub
  rw [List.filterMap_map, List.filterMap_map] at hsub
  exact hsub

theorem taggedFromSourced_preserves_distinct_sources (originals : List LoadedRow)
    (sourced : List SourcedRow) (emitted : List TaggedRow)
    (h : taggedFromSourced originals sourced = .ok emitted)
    (hdistinct : (originals.filterMap LoadedRow.source).Nodup) :
    (emitted.filterMap TaggedRow.source).Nodup :=
  hdistinct.sublist (taggedFromSourced_emitted_sources_sublist originals sourced emitted h)

/-! ## Block-index composition and equivalence with `selectReplayBlocks` -/

theorem mapBlockIndices_length (originalIndices positions composed : List Nat)
    (h : mapBlockIndices originalIndices positions = some composed) :
    composed.length = positions.length := by
  induction positions generalizing composed with
  | nil =>
      simp [mapBlockIndices] at h
      subst h
      rfl
  | cons pos rest ih =>
      simp only [mapBlockIndices, Option.bind_eq_bind] at h
      cases hget : originalIndices[pos]? with
      | none => simp [hget] at h
      | some orig =>
          simp only [hget, Option.some_bind] at h
          cases htail : mapBlockIndices originalIndices rest with
          | none => simp [htail] at h
          | some tail =>
              simp only [htail, Option.some_bind, Option.some.injEq] at h
              subst h
              simp [ih _ htail]

theorem mapBlockIndices_zip_lookup (originalIndices positions composed : List Nat)
    (h : mapBlockIndices originalIndices positions = some composed) :
    ∀ pair ∈ positions.zip composed, originalIndices[pair.1]? = some pair.2 := by
  induction positions generalizing composed with
  | nil =>
      intro pair hmem
      simp at hmem
  | cons pos rest ih =>
      simp only [mapBlockIndices, Option.bind_eq_bind] at h
      cases hget : originalIndices[pos]? with
      | none => simp [hget] at h
      | some orig =>
          simp only [hget, Option.some_bind] at h
          cases htail : mapBlockIndices originalIndices rest with
          | none => simp [htail] at h
          | some tail =>
              simp only [htail, Option.some_bind, Option.some.injEq] at h
              subst h
              intro pair hmem
              simp only [List.zip_cons_cons, List.mem_cons] at hmem
              rcases hmem with rfl | hrest
              · exact hget
              · exact ih _ htail pair hrest

theorem mapBlockIndices_sublist (originalIndices positions composed : List Nat)
    (hposInc : List.Pairwise (· < ·) positions)
    (h : mapBlockIndices originalIndices positions = some composed) :
    List.Sublist composed originalIndices :=
  sublist_of_indexed_strictly_increasing originalIndices positions composed hposInc
    (mapBlockIndices_zip_lookup originalIndices positions composed h)
    (mapBlockIndices_length originalIndices positions composed h).symm

theorem mapBlockIndices_strictly_increasing (originalIndices positions composed : List Nat)
    (horigInc : List.Pairwise (· < ·) originalIndices)
    (hposInc : List.Pairwise (· < ·) positions)
    (h : mapBlockIndices originalIndices positions = some composed) :
    List.Pairwise (· < ·) composed :=
  horigInc.sublist (mapBlockIndices_sublist originalIndices positions composed hposInc h)

private theorem filter_mem_sublist_of_nodup {α : Type} [DecidableEq α] :
    ∀ {sub xs : List α}, List.Sublist sub xs → xs.Nodup →
      xs.filter (fun x => decide (x ∈ sub)) = sub := by
  intro sub xs hsub hnodup
  induction hsub with
  | slnil => rfl
  | @cons l₁ l₂ a hsub ih =>
      have hcons := List.nodup_cons.mp hnodup
      have hnotmem : a ∉ l₁ := fun ha => hcons.1 (hsub.subset ha)
      simp [List.filter_cons, hnotmem, ih hcons.2]
  | @cons₂ l₁ l₂ a hsub ih =>
      have hcons := List.nodup_cons.mp hnodup
      have hfilt : l₂.filter (fun x => decide (x = a ∨ x ∈ l₁)) =
          l₂.filter (fun x => decide (x ∈ l₁)) := by
        apply List.filter_congr
        intro x hx
        have hne : x ≠ a := fun heq => hcons.1 (heq ▸ hx)
        simp [hne]
      simp only [List.filter_cons, List.mem_cons, true_or, decide_true, ↓reduceIte]
      rw [hfilt, ih hcons.2]

/-- Composing strictly increasing projected positions against a strictly increasing
original block-index list agrees identically with filtering the original block
indices by membership (`select_tagged_assistant_blocks` / `ClaudeMap.selectReplayBlocks`). -/
theorem mapBlockIndices_eq_filter_selection (originalIndices positions composed : List Nat)
    (horigInc : strictlyIncreasingIndices originalIndices = true)
    (hposInc : strictlyIncreasingIndices positions = true)
    (h : mapBlockIndices originalIndices positions = some composed) :
    originalIndices.filter (fun idx => decide (idx ∈ composed)) = composed := by
  have horigPair : List.Pairwise (· < ·) originalIndices :=
    of_decide_eq_true (by simpa [strictlyIncreasingIndices] using horigInc)
  have hposPair : List.Pairwise (· < ·) positions :=
    of_decide_eq_true (by simpa [strictlyIncreasingIndices] using hposInc)
  have hnodup : originalIndices.Nodup :=
    horigPair.imp fun hab => Nat.ne_of_lt hab
  exact filter_mem_sublist_of_nodup
    (mapBlockIndices_sublist originalIndices positions composed hposPair h) hnodup

/-! ## Identity projection roundtrip -/

def LoadedRow.toTagged (row : LoadedRow) : TaggedRow :=
  { role := row.role
    source := row.source
    physicalHeader := row.physicalHeader
    blockIndices := if row.role == .assistant then row.blockIndices else []
    stubReceiptKeys := if row.role == .assistant then [] else row.stubReceiptKeys }

def identitySourcedFrom (offset : Nat) : List LoadedRow → List SourcedRow
  | [] => []
  | row :: rest =>
      { sourceIndex := offset
        role := row.role
        blockCount := if row.role == .assistant then row.blockCount else 0
        blockIndices := if row.role == .assistant then List.range row.blockCount else []
        keptToolResultKeys := if row.role == .assistant then [] else row.stubReceiptKeys } ::
      identitySourcedFrom (offset + 1) rest

def identitySourced (originals : List LoadedRow) : List SourcedRow :=
  identitySourcedFrom 0 originals

def loadedRowWellFormed (row : LoadedRow) : Bool :=
  match row.role with
  | .assistant =>
      isUnhydrated row || originalAssociationValid row
  | _ => row.source.isNone

private theorem mapBlockIndices_range_from (xs : List Nat) :
    ∀ (k : Nat), k ≤ xs.length →
      mapBlockIndices xs ((List.range (xs.length - k)).map (· + k)) = some (xs.drop k) := by
  intro k hk
  generalize hn : xs.length - k = n
  induction n generalizing k with
  | zero =>
      have heq : k = xs.length := by omega
      subst heq
      simp [mapBlockIndices]
  | succ n ih =>
      have hlt : k < xs.length := by omega
      have hrange : (List.range (n + 1)).map (· + k) =
          k :: (List.range n).map (· + (k + 1)) := by
        rw [List.range_succ_eq_map, List.map_cons, List.map_map]
        simp only [Nat.zero_add, List.cons.injEq, true_and]
        apply List.map_congr_left
        intro a _
        simp [Function.comp]
        omega
      rw [hrange]
      simp only [mapBlockIndices, Option.bind_eq_bind]
      have hget : xs[k]? = some xs[k] := List.getElem?_eq_getElem hlt
      rw [hget]
      simp only [Option.some_bind]
      have htail := ih (k + 1) (by omega) (by omega)
      rw [htail]
      simp only [Option.some_bind, Option.some.injEq]
      exact List.cons_getElem_drop_succ (h := hlt)

private theorem mapBlockIndices_range (xs : List Nat) :
    mapBlockIndices xs (List.range xs.length) = some xs := by
  have h := mapBlockIndices_range_from xs 0 (Nat.zero_le _)
  simpa using h

private theorem strictlyIncreasing_range (n : Nat) :
    strictlyIncreasingIndices (List.range n) = true := by
  simp only [strictlyIncreasingIndices, decide_eq_true_eq]
  exact List.pairwise_lt_range n

private theorem tagOneRow_identity (row : LoadedRow) (hwf : loadedRowWellFormed row = true) :
    tagOneRow row
      { sourceIndex := 0
        role := row.role
        blockCount := if row.role == .assistant then row.blockCount else 0
        blockIndices := if row.role == .assistant then List.range row.blockCount else []
        keptToolResultKeys := if row.role == .assistant then [] else row.stubReceiptKeys } =
      .ok row.toTagged := by
  cases hr : row.role with
  | assistant =>
      simp only [loadedRowWellFormed, hr, Bool.or_eq_true] at hwf
      unfold tagOneRow LoadedRow.toTagged
      simp only [hr, beq_self_eq_true, bne_self_eq_false, Bool.and_false, ↓reduceIte,
        List.length_range, strictlyIncreasing_range, Bool.not_true]
      rcases hwf with hunh | hval
      · simp only [hunh, ↓reduceIte]
        have hall : (List.range row.blockCount).all (· < row.blockCount) = true := by
          rw [List.all_eq_true]
          intro x hx
          exact decide_eq_true (List.mem_range.mp hx)
        simp only [isUnhydrated, Bool.and_eq_true, Option.isNone_iff_eq_none,
          List.isEmpty_iff] at hunh
        simp [hall, hunh.1, hunh.2]
      · by_cases hunh : isUnhydrated row = true
        · simp only [hunh, ↓reduceIte]
          have hall : (List.range row.blockCount).all (· < row.blockCount) = true := by
            rw [List.all_eq_true]
            intro x hx
            exact decide_eq_true (List.mem_range.mp hx)
          simp only [isUnhydrated, Bool.and_eq_true, Option.isNone_iff_eq_none,
            List.isEmpty_iff] at hunh
          simp [hall, hunh.1, hunh.2]
        · simp only [hunh, ↓reduceIte]
          have hlen : row.blockIndices.length = row.blockCount := by
            simp only [originalAssociationValid, hr, beq_self_eq_true, Bool.true_and,
              Bool.and_eq_true, beq_iff_eq] at hval
            exact hval.1
          rw [← hlen, mapBlockIndices_range]
          simp [hval]
  | user =>
      simp only [loadedRowWellFormed, hr, Option.isNone_iff_eq_none] at hwf
      unfold tagOneRow LoadedRow.toTagged
      have hfilt : row.stubReceiptKeys.filter (fun key => row.stubReceiptKeys.contains key) =
          row.stubReceiptKeys := by
        rw [List.filter_eq_self]
        intro key hkey
        exact List.contains_iff_mem.mpr hkey
      simp [hr, hwf, hfilt]
  | system =>
      simp only [loadedRowWellFormed, hr, Option.isNone_iff_eq_none] at hwf
      unfold tagOneRow LoadedRow.toTagged
      have hfilt : row.stubReceiptKeys.filter (fun key => row.stubReceiptKeys.contains key) =
          row.stubReceiptKeys := by
        rw [List.filter_eq_self]
        intro key hkey
        exact List.contains_iff_mem.mpr hkey
      simp [hr, hwf, hfilt]

private theorem taggedFromSourcedLoop_identity (originals : List LoadedRow) :
    ∀ (offset : Nat) (prev : Option Nat) (suffix : List LoadedRow),
      originals.drop offset = suffix →
      (∀ p, prev = some p → p < offset) →
      suffix.all loadedRowWellFormed = true →
      taggedFromSourcedLoop originals prev (identitySourcedFrom offset suffix) =
        .ok (suffix.map LoadedRow.toTagged) := by
  intro offset prev suffix hdrop hprev hwf
  induction suffix generalizing offset prev with
  | nil => simp [identitySourcedFrom, taggedFromSourcedLoop]
  | cons row rest ih =>
      simp only [List.all_cons, Bool.and_eq_true] at hwf
      have hget : originals[offset]? = some row := by
        have hlen : offset < originals.length := by
          by_contra hge
          have : originals.drop offset = [] := List.drop_eq_nil_of_le (Nat.le_of_not_lt hge)
          rw [this] at hdrop
          cases hdrop
        have hcons := List.cons_getElem_drop_succ (h := hlen)
        rw [hdrop] at hcons
        have := (List.cons.inj hcons).1
        rw [List.getElem?_eq_getElem hlen, this]
      have hdropNext : originals.drop (offset + 1) = rest := by
        have hlen : offset < originals.length := by
          by_contra hge
          have : originals.drop offset = [] := List.drop_eq_nil_of_le (Nat.le_of_not_lt hge)
          rw [this] at hdrop
          cases hdrop
        have hcons := List.cons_getElem_drop_succ (h := hlen)
        rw [hdrop] at hcons
        exact (List.cons.inj hcons).2
      have hnotPrev : prev.any (fun p => decide (offset ≤ p)) = false := by
        cases prev with
        | none => rfl
        | some p =>
            have hlt := hprev p rfl
            simp only [Option.any_some, decide_eq_false_iff_not, Nat.not_le]
            exact hlt
      have htag : tagOneRow row
          { sourceIndex := offset
            role := row.role
            blockCount := if row.role == .assistant then row.blockCount else 0
            blockIndices := if row.role == .assistant then List.range row.blockCount else []
            keptToolResultKeys := if row.role == .assistant then [] else row.stubReceiptKeys } =
          .ok row.toTagged := by
        simpa [tagOneRow] using tagOneRow_identity row hwf.1
      unfold identitySourcedFrom taggedFromSourcedLoop
      simp only [hnotPrev, Bool.false_eq_true, ↓reduceIte, hget, htag]
      have htail := ih (offset + 1) (some offset) hdropNext
        (fun p hp => by cases hp; omega) hwf.2
      simp [htail]

theorem taggedFromSourced_identity_roundtrip (originals : List LoadedRow)
    (hwf : originals.all loadedRowWellFormed = true) :
    taggedFromSourced originals (identitySourced originals) =
      .ok (originals.map LoadedRow.toTagged) := by
  have hnoBad : originals.any (fun row => row.source.isSome && row.role != .assistant) = false := by
    rw [List.any_eq_false]
    intro row hmem
    have hrow := (List.all_eq_true.mp hwf) row hmem
    cases hr : row.role with
    | assistant => simp [hr]
    | user =>
        simp only [loadedRowWellFormed, hr, Option.isNone_iff_eq_none] at hrow
        simp [hr, hrow]
    | system =>
        simp only [loadedRowWellFormed, hr, Option.isNone_iff_eq_none] at hrow
        simp [hr, hrow]
  unfold taggedFromSourced identitySourced
  simp only [hnoBad, Bool.false_eq_true, ↓reduceIte]
  exact taggedFromSourcedLoop_identity originals 0 none originals (by simp) (by intro _ h; cases h) hwf

/-- Two identical assistant occurrences at distinct loaded indices keep their
respective source tags in order. -/
theorem equal_assistant_occurrences_keep_distinct_tags :
    let tag1 : ReplayTag := { request := 1, source := .provider 1 1 0 }
    let tag2 : ReplayTag := { request := 1, source := .provider 2 2 0 }
    let originals : List LoadedRow :=
      [ { role := .assistant, source := some tag1, blockCount := 1 }
      , { role := .user }
      , { role := .assistant, source := some tag2, blockCount := 1 } ]
    taggedFromSourced originals (identitySourced originals) =
      .ok [ { role := .assistant, source := some tag1 }
          , { role := .user }
          , { role := .assistant, source := some tag2 } ] := rfl

end PromptAssembly.SourceTagging
