import Mathlib.Data.List.Basic

/-!
# Query selections across relationships

`query` reads one collection's rows. A requested field may follow one
relationship to fields of the related documents (`writer.name`,
`stories.headline`), and `search` ranks the rows by BM25 keywords or by
vector similarity. The decisions rest on what the call observes in the native
schema: what each segment of a requested path names, and whether a
relationship's target collection is readable under the call's datastore
grant (inside it and not protected).

External premise (the pinned DefraDB, evidenced for selections by
`relationship_selections_follow_grants_and_acp`): a relationship selection is
answered by the same read as its rows, inside the call's one read-only
transaction, and the native planner filters the related documents by ACP for
the reading identity as it filters the rows. A related document the reader may
not read is `null` (one) or absent (list). The read decides nothing that is
written, so it needs no commit fence (`TransactionFence`): the answer is what
that transaction observed.

A filter condition that crosses a relationship reads the related documents
too, so it is admitted only when `crossable` holds for that relationship.
External premise, the exception: the pinned DefraDB evaluates a condition on
a relationship the read does not also select (its filter-only join) without
that ACP filter, so such a condition can match on a related document the
reader may not read; within the grant, nothing here narrows it.
-/

namespace ToolPolicy.QuerySelection

/-- What one segment of a requested field path names in the native schema. -/
inductive Kind where
  /-- A stored value: a scalar, a list of scalars, JSON, or a relationship's
  id field. -/
  | value
  /-- A relationship to one document or a list of documents of another
  collection. -/
  | relation
  /-- No field the caller may name: absent, restricted, or internal. -/
  | unavailable
  deriving DecidableEq, Repr

/-- One observed segment. `granted` is meaningful for a relationship: its
target collection is within the call's grant and not protected. -/
structure Segment where
  kind : Kind
  granted : Bool
  deriving DecidableEq, Repr

inductive Decision where
  | select
  | unavailable
  | notARelation
  | outsideGrant
  | needsField
  | tooDeep
  deriving DecidableEq, Repr

/-- How many relationships one requested field may cross. One hop answers
the question a caller asks after linking documents (which writer, which
stories) while each row's related documents stay bounded by `listLimit`;
a deeper path multiplies those bounds. -/
def maxRelations : Nat := 1

/-- The related documents a list relationship shows per row. Production
reads one more to observe that more exist. -/
def listLimit : Nat := 20

/-- Whether a read may cross this relationship. -/
def crossable (s : Segment) : Bool := s.kind == .relation && s.granted

/-- The decision for the rest of a path after `crossed` relationships, with
the index of the segment that decided. -/
def walk (crossed index : Nat) : List Segment → Decision × Nat
  | [] => (.needsField, index)
  | s :: rest =>
    match s.kind with
    | .unavailable => (.unavailable, index)
    | .value => if rest.isEmpty then (.select, index) else (.notARelation, index)
    | .relation =>
      if !s.granted then (.outsideGrant, index)
      else if rest.isEmpty then (.needsField, index)
      else if crossed + 1 > maxRelations then (.tooDeep, index)
      else walk (crossed + 1) (index + 1) rest

def decide (segments : List Segment) : Decision × Nat :=
  if segments.isEmpty then (.unavailable, 0) else walk 0 0 segments

/-- What a list relationship shows of the `observed` related documents (at
most `listLimit + 1` are read), and whether more exist than it shows. -/
def relatedPage (observed : Nat) : Nat × Bool :=
  (min observed listLimit, listLimit < observed)

inductive Ranking where
  | bm25
  | similarity
  | refused
  deriving DecidableEq, Repr

/-- `search` ranks by keywords (`text` with `search_fields`) or by a vector
(`vector` with `vector_field`), never both and never neither: one score
orders the rows. -/
def ranking (keywords vector : Bool) : Ranking :=
  match keywords, vector with
  | true, false => .bm25
  | false, true => .similarity
  | _, _ => .refused

theorem walk_select_crosses_only_granted (crossed index : Nat) (segments : List Segment)
    (h : (walk crossed index segments).1 = .select) :
    ∀ s ∈ segments, s.kind = .relation → s.granted = true := by
  induction segments generalizing crossed index with
  | nil => simp [walk] at h
  | cons s rest ih =>
    intro t ht hk
    rcases List.mem_cons.mp ht with rfl | hmem
    · cases hs : t.kind with
      | value => simp [hs] at hk
      | unavailable => simp [hs] at hk
      | relation =>
        cases hg : t.granted
        · simp [walk, hs, hg] at h
        · rfl
    · cases hs : s.kind with
      | unavailable => simp [walk, hs] at h
      | value =>
        cases hr : rest.isEmpty
        · simp [walk, hs, hr] at h
        · simp [List.isEmpty_iff] at hr; simp [hr] at hmem
      | relation =>
        by_cases hg : s.granted = true
        · cases hr : rest.isEmpty
          · by_cases hd : crossed + 1 > maxRelations
            · simp [walk, hs, hg, hr, hd] at h
            · simp [walk, hs, hg, hr, hd] at h
              exact ih (crossed + 1) (index + 1) h t hmem hk
          · simp [walk, hs, hg, hr] at h
        · simp [walk, hs, hg] at h

/-- A selected path reads only collections the call's grant admits. -/
theorem a_selection_crosses_only_granted_relationships (segments : List Segment)
    (h : (decide segments).1 = .select) :
    ∀ s ∈ segments, s.kind = .relation → s.granted = true := by
  unfold decide at h
  split at h
  · simp at h
  · exact walk_select_crosses_only_granted 0 0 segments h

theorem walk_select_never_unavailable (crossed index : Nat) (segments : List Segment)
    (h : (walk crossed index segments).1 = .select) :
    ∀ s ∈ segments, s.kind ≠ .unavailable := by
  induction segments generalizing crossed index with
  | nil => simp [walk] at h
  | cons s rest ih =>
    intro t ht
    cases hs : s.kind with
    | unavailable => simp [walk, hs] at h
    | value =>
      cases hr : rest.isEmpty
      · simp [walk, hs, hr] at h
      · simp [List.isEmpty_iff] at hr
        subst hr
        simp at ht
        subst ht
        simp [hs]
    | relation =>
      by_cases hg : s.granted = true
      · cases hr : rest.isEmpty
        · by_cases hd : crossed + 1 > maxRelations
          · simp [walk, hs, hg, hr, hd] at h
          · simp [walk, hs, hg, hr, hd] at h
            rcases List.mem_cons.mp ht with rfl | hmem
            · simp [hs]
            · exact ih (crossed + 1) (index + 1) h t hmem
        · simp [walk, hs, hg, hr] at h
      · simp [walk, hs, hg] at h

/-- An absent, restricted or internal field is never read, at any segment. -/
theorem an_unavailable_field_is_never_selected (segments : List Segment)
    (s : Segment) (hs : s ∈ segments) (hk : s.kind = .unavailable) :
    (decide segments).1 ≠ .select := by
  intro h
  unfold decide at h
  split at h
  · simp at h
  · exact walk_select_never_unavailable 0 0 segments h s hs hk

/-- A path that ends on a relationship names no field to read. -/
theorem a_bare_relationship_needs_a_field (s : Segment) (hk : s.kind = .relation)
    (hg : s.granted = true) : decide [s] = (.needsField, 0) := by
  simp [decide, walk, hk, hg]

/-- Two relationships deep is refused. -/
theorem two_relationships_are_too_deep (a b leaf : Segment) (ha : a.kind = .relation)
    (hga : a.granted = true) (hb : b.kind = .relation) (hgb : b.granted = true) :
    (decide [a, b, leaf]).1 = .tooDeep := by
  simp [decide, walk, ha, hga, hb, hgb, maxRelations]

/-- A list relationship never shows more than `listLimit`, and says when it
holds back. -/
theorem a_related_page_is_bounded_and_marked (observed : Nat) :
    (relatedPage observed).1 ≤ listLimit ∧
      ((relatedPage observed).2 = true ↔ (relatedPage observed).1 < observed) := by
  unfold relatedPage
  refine ⟨Nat.min_le_right _ _, ?_⟩
  constructor
  · intro h
    simp only [decide_eq_true_eq] at h
    omega
  · intro h
    simp only [decide_eq_true_eq]
    omega

end ToolPolicy.QuerySelection
