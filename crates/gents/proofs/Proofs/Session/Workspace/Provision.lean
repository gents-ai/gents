import Proofs.Session.Workspace.Types

/-! Creating an isolated workspace on its host. The destination must stay under
the configured ceiling before anything is read or written. An admitted row with
another logical identity is never overwritten. A destination that already holds
something other than this workspace's checkout is recorded as
`provisionFailed` and left exactly as found: no checkout over it and no cleanup
of it. Dirty-base evidence lives on the node-local placement, never on the
replicated workspace row. -/

namespace Workspace.Provision

inductive Result where
  | ready | denied | failed
  deriving DecidableEq, Repr

/-- What the planned destination holds. -/
inductive Target where
  | absent | foreign | checkout
  deriving DecidableEq, Repr

structure Row where
  state : WorkspaceState
  workUnit : Nat
  deriving DecidableEq, Repr

structure Situation where
  /-- The row an earlier successful creation admitted, if any. -/
  prior : Option Row
  workUnit : Nat
  target : Target
  ceilingEscapes : Bool
  deriving DecidableEq, Repr

structure Observation where
  result : Result
  identityMismatch : Bool
  outcomeState : Option WorkspaceState
  stored : Option Row
  target : Target
  rowCarriesDirtyBase : Bool
  deriving DecidableEq, Repr

def rowCarriesDirtyBase (_ : Option Row) : Bool := false

def create (s : Situation) : Observation :=
  let unchanged (result : Result) (mismatch : Bool) (outcome : Option WorkspaceState) :=
    { result, identityMismatch := mismatch, outcomeState := outcome, stored := s.prior
      target := s.target, rowCarriesDirtyBase := rowCarriesDirtyBase s.prior : Observation }
  if s.ceilingEscapes then unchanged .denied false none
  else match s.prior with
  | some row =>
      if row.workUnit ≠ s.workUnit then unchanged .failed true none
      else
        let stored := some { row with state := .ready }
        { result := .ready, identityMismatch := false, outcomeState := some .ready, stored
          target := .checkout, rowCarriesDirtyBase := rowCarriesDirtyBase stored }
  | none => match s.target with
    | .foreign =>
        let stored := some ⟨.provisionFailed, s.workUnit⟩
        { result := .failed, identityMismatch := true, outcomeState := some .provisionFailed
          stored, target := .foreign, rowCarriesDirtyBase := rowCarriesDirtyBase stored }
    | _ =>
        let stored := some ⟨.ready, s.workUnit⟩
        { result := .ready, identityMismatch := false, outcomeState := some .ready, stored
          target := .checkout, rowCarriesDirtyBase := rowCarriesDirtyBase stored }

theorem existing_target_mismatch_does_not_overwrite_or_cleanup (workUnit : Nat) :
    create ⟨none, workUnit, .foreign, false⟩ =
      { result := .failed, identityMismatch := true, outcomeState := some .provisionFailed
        stored := some ⟨.provisionFailed, workUnit⟩, target := .foreign
        rowCarriesDirtyBase := false } := rfl

theorem matching_ready_workspace_is_not_overwritten_by_identity_mismatch_retry
    (prior workUnit : Nat) (h : prior ≠ workUnit) :
    create ⟨some ⟨.ready, prior⟩, workUnit, .checkout, false⟩ =
      { result := .failed, identityMismatch := true, outcomeState := none
        stored := some ⟨.ready, prior⟩, target := .checkout, rowCarriesDirtyBase := false } := by
  simp [create, h, rowCarriesDirtyBase]

theorem destination_escaping_ceiling_is_denied (s : Situation) (h : s.ceilingEscapes = true) :
    (create s).result = .denied ∧ (create s).stored = s.prior ∧ (create s).target = s.target := by
  simp [create, h]

end Workspace.Provision
