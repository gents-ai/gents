import Proofs.Store.Views
import Mathlib.Data.List.Sort

namespace AgentViews

structure Row where
  agentDid : String
  nodeDid : String
  readable : Bool
  deriving DecidableEq, Repr

structure Request where
  nodeDid : String
  reader : Option String
  agentDid : Option String
  after : Option String
  limit : Option Nat
  deriving Repr

def selected (request : Request) (row : Row) : Bool :=
  (request.agentDid.all (· == row.agentDid)) &&
    (request.after.all (· < row.agentDid))

def inventory (request : Request) (documents : List Row) : List Row :=
  let visible := ReadViews.rows .ownNode request.nodeDid request.reader
    Row.nodeDid Row.agentDid (fun _ row => row.readable) documents
  let ordered := (visible.filter (selected request)).mergeSort
    (fun left right => left.agentDid ≤ right.agentDid)
  match request.limit with
  | none => ordered
  | some count => ordered.take count

theorem inventory_rows_are_readable (request : Request) (documents : List Row)
    (row : Row) (member : row ∈ inventory request documents) : row.readable = true := by
  cases limit : request.limit with
  | none =>
    simp only [inventory, limit, List.mem_mergeSort, List.mem_filter] at member
    obtain ⟨actor, _, readable⟩ :=
      ReadViews.rows_subset_readable _ _ _ _ _ _ _ _ member.1
    exact readable
  | some count =>
    simp only [inventory, limit] at member
    have retained := List.mem_of_mem_take member
    simp only [List.mem_mergeSort, List.mem_filter] at retained
    obtain ⟨actor, _, readable⟩ :=
      ReadViews.rows_subset_readable _ _ _ _ _ _ _ _ retained.1
    exact readable

end AgentViews
