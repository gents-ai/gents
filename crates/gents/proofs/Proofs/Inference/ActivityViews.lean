import Proofs.Store.Views
import Proofs.Inference.InferenceCall.State

namespace InferenceActivityViews

open Lean

deriving instance ToJson for InferenceCallState

def callStateName (state : InferenceCallState) : String := state.toDefraDB

inductive Mode where
  | active
  | completed
  deriving DecidableEq, Repr, ToJson

def selects (mode : Mode) (state : InferenceCallState)
    (kindEligible insideWindow : Bool) : Bool :=
  match mode with
  | .active => decide (state = .queued ∨ state = .running)
  | .completed => decide (isTerminal state) && kindEligible && insideWindow

def included (mode : Mode) (state : InferenceCallState)
    (readable sameNode kindEligible insideWindow : Bool) : Bool :=
  ReadViews.visible .ownNode "node" "reader"
    (if sameNode then "node" else "other-node") "agent" readable &&
    selects mode state kindEligible insideWindow

theorem included_requires_readable (mode : Mode) (state : InferenceCallState)
    (readable sameNode kindEligible insideWindow : Bool)
    (h : included mode state readable sameNode kindEligible insideWindow = true) :
    readable = true := by
  have selected := h
  simp only [included, Bool.and_eq_true] at selected
  exact ReadViews.visible_requires_readable _ _ _ _ _ _ selected.1

theorem included_stays_on_node (mode : Mode) (state : InferenceCallState)
    (readable sameNode kindEligible insideWindow : Bool)
    (h : included mode state readable sameNode kindEligible insideWindow = true) :
    sameNode = true := by
  cases sameNode <;> simp_all [included, ReadViews.visible, ReadViews.scopeMatches]

end InferenceActivityViews
