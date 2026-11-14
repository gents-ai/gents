import Proofs.Store.Views

namespace RequestViews

open Lean

inductive State where
  | absent
  | pending
  | completed
  deriving DecidableEq, Repr, ToJson

def seed (completed missingState retryParent retryParentDocument successor onPage : Bool) : Bool :=
  !completed || missingState || retryParent || retryParentDocument || successor || onPage

theorem seed_retains_noncompleted (missingState retryParent retryParentDocument successor onPage : Bool) :
    seed false missingState retryParent retryParentDocument successor onPage = true := by
  simp [seed]

theorem seed_retains_page (completed missingState retryParent retryParentDocument successor : Bool) :
    seed completed missingState retryParent retryParentDocument successor true = true := by
  simp [seed]

def visibleSeed (readable sameNode completed missingState retryParent retryParentDocument successor onPage : Bool) : Bool :=
  ReadViews.visible .ownNode "node" "reader" (if sameNode then "node" else "other") "" readable &&
    seed completed missingState retryParent retryParentDocument successor onPage

theorem visible_seed_requires_reader_access
    (readable sameNode completed missingState retryParent retryParentDocument successor onPage : Bool)
    (included : visibleSeed readable sameNode completed missingState retryParent retryParentDocument successor onPage = true) :
    readable = true := by
  simp only [visibleSeed, Bool.and_eq_true] at included
  exact ReadViews.visible_requires_readable _ _ _ _ _ _ included.1

def observedSeed (state : State) (readable sameNode retryParent retryParentDocument successor onPage : Bool) : Bool :=
  visibleSeed readable sameNode (state == .completed) (state == .absent)
    retryParent retryParentDocument successor onPage

def causalChild (readable anchored public hasTool : Bool) : Bool :=
  readable && anchored && public && hasTool

theorem causal_child_requires_reader_and_parent (readable anchored public hasTool : Bool)
    (included : causalChild readable anchored public hasTool = true) :
    readable = true ∧ anchored = true := by
  simp only [causalChild, Bool.and_eq_true] at included
  exact included.1.1

end RequestViews
