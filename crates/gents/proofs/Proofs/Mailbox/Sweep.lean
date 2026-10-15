import Proofs.Mailbox.Transition

namespace Mailbox.Sweep

/-- The read owner sees a finite ordered page of open physical rows. `docId`
orders the keyset; node ownership scopes the reader. -/
structure Row where
  docId : Nat
  nodeDid : String
  isOpen : Bool
  due : Bool
  deriving DecidableEq, Repr

structure State where
  rows : List Row
  cursor : Option Nat
  deriving DecidableEq, Repr

def eligible (state : State) (nodeDid : String) : List Row :=
  state.rows.filter fun row =>
    row.isOpen && row.nodeDid = nodeDid && state.cursor.all (row.docId > ·)

def selected (state : State) (nodeDid : String) (pageSize budget : Nat) : List Row :=
  ((eligible state nodeDid).take pageSize).take budget

def lastDocId? (rows : List Row) : Option Nat :=
  rows.getLast?.map Row.docId

/-- One observer turn is bounded by both the database page and completed-row
budget. When more eligible rows remain, the cursor names the last processed
row; exhaustion wraps to the beginning on the next tick. -/
def nextCursor (state : State) (nodeDid : String) (pageSize budget : Nat) : Option Nat :=
  let available := eligible state nodeDid
  let batch := selected state nodeDid pageSize budget
  if batch.isEmpty then none
  else if batch.length < available.length then lastDocId? batch
  else none

def closeSelected (state : State) (batch : List Row) : State :=
  { state with rows := state.rows.map fun row =>
      if row.due && batch.any (fun selectedRow => selectedRow.docId = row.docId) then
        { row with isOpen := false }
      else row }

def advance (state : State) (nodeDid : String) (pageSize budget : Nat) : State :=
  let batch := selected state nodeDid pageSize budget
  { (closeSelected state batch) with cursor := nextCursor state nodeDid pageSize budget }

/-- A failed read has not established a page and therefore cannot advance the
cursor or claim any row as processed. -/
def readFailure (state : State) : State := state

private theorem mem_of_mem_take {α : Type} [DecidableEq α] (row : α) :
    ∀ (count : Nat) (rows : List α), row ∈ rows.take count → row ∈ rows := by
  intro count
  induction count with
  | zero =>
      intro rows h
      simp at h
  | succ count ih =>
      intro rows h
      cases rows with
      | nil => simp at h
      | cons head tail =>
          simp only [List.take_succ_cons, List.mem_cons] at h ⊢
          rcases h with h | h
          · exact Or.inl h
          · exact Or.inr (ih tail h)

theorem selected_is_owner_scoped (state : State) (nodeDid : String)
    (pageSize budget : Nat) (row : Row) (h : row ∈ selected state nodeDid pageSize budget) :
    row.nodeDid = nodeDid := by
  have hEligible : row ∈ eligible state nodeDid :=
    mem_of_mem_take row _ _ (mem_of_mem_take row _ _ h)
  simp only [eligible, List.mem_filter] at hEligible
  rcases hEligible with ⟨_, hPredicate⟩
  simp only [Bool.and_eq_true_eq_eq_true_and_eq_true] at hPredicate
  exact of_decide_eq_true hPredicate.1.2

theorem selected_respects_page_and_budget (state : State) (nodeDid : String)
    (pageSize budget : Nat) :
    (selected state nodeDid pageSize budget).length ≤ pageSize ∧
      (selected state nodeDid pageSize budget).length ≤ budget := by
  simp [selected, Nat.min_le_left, Nat.min_le_right]

theorem read_failure_keeps_cursor (state : State) :
    (readFailure state).cursor = state.cursor := rfl

theorem keyset_progress_survives_close :
    let state : State :=
      { rows := [⟨1, "local", true, true⟩, ⟨2, "local", true, true⟩,
        ⟨3, "local", true, true⟩, ⟨4, "local", true, true⟩,
        ⟨5, "foreign", true, true⟩]
        cursor := none }
    let first := advance state "local" 2 2
    let second := advance first "local" 2 2
    (first.cursor = some 2) ∧
      (selected first "local" 2 2).map Row.docId = [3, 4] ∧
      (second.cursor = none) ∧
      ((second.rows.find? (fun row => row.docId = 5)).map Row.isOpen = some true) := by
  decide

end Mailbox.Sweep
