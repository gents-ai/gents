import Proofs.Store.Views

namespace TranscriptViews

open Lean

def visible (readable sameNode sameRequester sameSession sameRequest : Bool) : Bool :=
  readable && sameNode && sameRequester && sameSession && sameRequest

def rows {α : Type} (source : List α) (readable inScope : α → Bool) : List α :=
  source.filter fun row => readable row && inScope row

theorem rows_subset_readable {α : Type} (source : List α) (readable inScope : α → Bool)
    (row : α) (included : row ∈ rows source readable inScope) : readable row = true := by
  have selected := (List.mem_filter.mp included).2
  simp only [Bool.and_eq_true] at selected
  exact selected.1

inductive Window where
  | all
  | before
  | at
  | after
  deriving DecidableEq, Repr, ToJson

def within (window : Window) (sequence : Nat) : Bool :=
  match window with
  | .all => true
  | .before => sequence < 2
  | .at => sequence == 2
  | .after => sequence > 2

def window (selection : Window) : List Nat :=
  [1, 2, 3].filter (within selection)

inductive Source where
  | inference
  | title
  | tool
  | authored
  deriving DecidableEq, Repr, ToJson

/-- The database compares indexed JSON leaves. Source discrimination narrows
the read; canonical reconstruction still checks complete source equality. -/
def sourceVisible (source : Source) (sameSource liveOnly : Bool) : Bool :=
  sameSource && (!liveOnly || source == .inference)


end TranscriptViews
