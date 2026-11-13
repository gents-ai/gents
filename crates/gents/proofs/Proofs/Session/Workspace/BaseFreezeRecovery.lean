import Proofs.Session.Workspace.Types

/-! Persistence of an operator base freeze. The sealed workspace row is written
before its placement, so a crash between them leaves a sealed row whose
placement does not record the base tree. A later freeze with a fresh journal
recognizes the persisted seal, repairs the placement, and returns it; once both
documents agree, a replay writes nothing. A freeze never writes a receipt and
never moves trunk. -/

namespace Workspace.BaseFreezeRecovery

inductive Journal where
  | validated | executing | effectObserved | resultDocsWritten
  deriving DecidableEq, Repr

structure Docs where
  state : WorkspaceState
  sealedAtBase : Bool
  placementAtBase : Bool
  workspaceWrites : Nat := 0
  placementWrites : Nat := 0
  receipts : Nat := 0
  trunkMoved : Bool := false
  deriving DecidableEq, Repr

structure Result where
  ok : Bool
  returnedPlacementAtBase : Option Bool
  journal : Journal
  deriving DecidableEq, Repr

/-- The workspace row is written first; `fault` fails the placement write that
follows it, after the write was attempted. -/
def persist (d : Docs) (fault : Bool) : Docs × Bool :=
  let written := { d with state := .sealed, sealedAtBase := true
                          workspaceWrites := d.workspaceWrites + 1
                          placementWrites := d.placementWrites + 1 }
  if fault then (written, false) else ({ written with placementAtBase := true }, true)

def settle (d : Docs) (fault : Bool) : Docs × Result :=
  let (next, ok) := persist d fault
  if ok then (next, ⟨true, some true, .resultDocsWritten⟩)
  else (next, ⟨false, none, .effectObserved⟩)

def freeze (d : Docs) (fault : Bool) : Docs × Result :=
  match d.state with
  | .sealed =>
      if !d.sealedAtBase then (d, ⟨false, none, .validated⟩)
      else if d.placementAtBase then (d, ⟨true, some true, .resultDocsWritten⟩)
      else settle d fault
  | .ready => settle d fault
  | _ => (d, ⟨false, none, .validated⟩)

theorem persist_keeps_receipts_and_trunk (d : Docs) (fault : Bool) :
    (persist d fault).1.receipts = d.receipts ∧ (persist d fault).1.trunkMoved = d.trunkMoved := by
  unfold persist
  cases fault <;> simp

theorem settle_keeps_receipts_and_trunk (d : Docs) (fault : Bool) :
    (settle d fault).1.receipts = d.receipts ∧ (settle d fault).1.trunkMoved = d.trunkMoved := by
  have h := persist_keeps_receipts_and_trunk d fault
  unfold settle
  revert h
  cases persist d fault with
  | mk next ok => cases ok <;> simp

theorem freeze_never_writes_receipts_or_trunk (d : Docs) (fault : Bool) :
    (freeze d fault).1.receipts = d.receipts ∧ (freeze d fault).1.trunkMoved = d.trunkMoved := by
  have h := settle_keeps_receipts_and_trunk d fault
  unfold freeze
  repeat' split
  all_goals first | exact h | simp

theorem operator_base_freeze_recovers_partial_documents_with_fresh_journal :
    let interrupted := freeze ⟨.ready, false, false, 0, 0, 0, false⟩ true
    let recovered := freeze interrupted.1 false
    let replay := freeze recovered.1 false
    interrupted.1.state = .sealed ∧ interrupted.1.sealedAtBase = true ∧
      interrupted.1.placementAtBase = false ∧ interrupted.2.journal = .effectObserved ∧
      recovered.2 = ⟨true, some true, .resultDocsWritten⟩ ∧ recovered.1.placementAtBase = true ∧
      replay.1.workspaceWrites = recovered.1.workspaceWrites ∧
      replay.1.placementWrites = recovered.1.placementWrites := by
  decide

end Workspace.BaseFreezeRecovery
