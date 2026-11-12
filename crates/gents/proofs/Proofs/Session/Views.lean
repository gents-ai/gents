import Proofs.Store.Views

namespace SessionViews

structure Row where
  readable : Bool
  sameNode : Bool
  sameRequester : Bool
  archived : Bool
  deriving Lean.ToJson

/-- List scope narrows DefraDB's reader-authorized rows. Archived history is
visible only when the caller explicitly selects it. -/
def visible (readable sameNode sameRequester archived includeArchived : Bool) : Bool :=
  readable && sameNode && sameRequester && (!archived || includeArchived)

def callerVisible (readable sameRequester : Bool) : Bool :=
  readable && sameRequester

theorem caller_visible_requires_reader_scope (readable sameRequester : Bool)
    (included : callerVisible readable sameRequester = true) :
    readable = true ∧ sameRequester = true := by
  simpa only [callerVisible, Bool.and_eq_true] using included

def rows (source : List Row) (includeArchived : Bool) : List Row :=
  source.filter fun row =>
    visible row.readable row.sameNode row.sameRequester row.archived includeArchived

theorem rows_subset_readable (source : List Row) (includeArchived : Bool)
    (row : Row) (member : row ∈ rows source includeArchived) : row.readable = true := by
  have selected := (List.mem_filter.mp member).2
  simp only [visible, Bool.and_eq_true] at selected
  exact selected.1.1.1

theorem rows_stay_in_scope (source : List Row) (includeArchived : Bool)
    (row : Row) (member : row ∈ rows source includeArchived) :
    row.sameNode = true ∧ row.sameRequester = true := by
  have selected := (List.mem_filter.mp member).2
  simp only [visible, Bool.and_eq_true] at selected
  exact ⟨selected.1.1.2, selected.1.2⟩

theorem default_hides_archived (source : List Row)
    (row : Row) (member : row ∈ rows source false) : row.archived = false := by
  have selected := (List.mem_filter.mp member).2
  simp only [visible, Bool.and_eq_true, Bool.or_false, Bool.not_eq_true] at selected
  cases archived : row.archived <;> simp_all


structure OrderingRow where
  sessionId : String
  createdAt : String
  lastActivityAt : String
  nodeDid : String
  requesterDid : String
  deriving Lean.ToJson

/-- Immutable creation and identity keys keep a page boundary stable while
session activity changes. Every session inventory uses this same order. -/
def before (left right : OrderingRow) : Bool :=
  if left.createdAt != right.createdAt then decide (right.createdAt < left.createdAt)
  else if left.sessionId != right.sessionId then decide (right.sessionId < left.sessionId)
  else if left.nodeDid != right.nodeDid then decide (right.nodeDid < left.nodeDid)
  else decide (right.requesterDid ≤ left.requesterDid)

def ordered (source : List OrderingRow) : List OrderingRow := source.mergeSort before

theorem ordered_subset (source : List OrderingRow) (row : OrderingRow)
    (member : row ∈ ordered source) : row ∈ source := by
  exact List.mem_mergeSort.mp member

def orderedSessionIds (source : List OrderingRow) : List String :=
  (ordered source).map OrderingRow.sessionId


end SessionViews
