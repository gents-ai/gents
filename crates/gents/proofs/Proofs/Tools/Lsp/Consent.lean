import Proofs.Tools.ToolPolicy.WorkspacePermissions

/-! A language-server edit asks workspace consent for every target before it
writes any of them. Under automatic review an edit inside the configured
workspace is routine; an edit outside it is reviewed, and the first denial
refuses the whole edit with nothing written. -/

namespace Lsp.Consent

open ToolPolicy.WorkspacePermissions

def editInput (index : Nat) (inside : Bool) : Input :=
  { current := ⟨0, 0⟩
    configuredWorkspace := 0
    mode := .autoReview
    action := index
    ceilingAllows := true
    crossesBoundary := !inside }

structure Progress where
  index : Nat
  reviews : Nat
  approved : Bool
  deriving DecidableEq, Repr

/-- `denyAt` names the review, counted from zero, that the reviewer denies. -/
def step (denyAt : Option Nat) (p : Progress) (inside : Bool) : Progress :=
  if !p.approved then p
  else
    let input := editInput p.index inside
    match route input with
    | .allow => { p with index := p.index + 1 }
    | .review =>
        let review := if denyAt = some p.reviews then Review.deny else Review.allow
        { index := p.index + 1
          reviews := p.reviews + 1
          approved := reviewedRoute input review ⟨input.current, input.action⟩ == .allow }
    | _ => { p with approved := false }

def consent (edits : List Bool) (denyAt : Option Nat) : Progress :=
  edits.foldl (step denyAt) ⟨0, 0, true⟩

/-- How many edits were written, or `none` when consent was refused. -/
def applied (edits : List Bool) (denyAt : Option Nat) : Option Nat :=
  if (consent edits denyAt).approved then some edits.length else none

/-- Each target's content after the call: changed only when every edit was
approved. -/
def written (edits : List Bool) (denyAt : Option Nat) : List Bool :=
  edits.map fun _ => (applied edits denyAt).isSome

theorem lsp_edit_inside_the_original_workspace_does_not_request_review :
    (consent [true] none).reviews = 0 ∧ applied [true] none = some 1 ∧
      written [true] none = [true] := by
  decide

theorem lsp_edit_consent_is_complete_before_the_first_write :
    (consent [false, false] (some 1)).reviews = 2 ∧ applied [false, false] (some 1) = none := by
  decide

end Lsp.Consent
