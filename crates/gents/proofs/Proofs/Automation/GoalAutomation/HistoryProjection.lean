import Proofs.GoalAutomation.RequestAssignment
import Lean

namespace GoalAutomation.HistoryProjection

open RequestAssignment

/-- DefraDB composite links identify the fields written by that revision.
An unchanged assignment projection may inherit only from its sole physical
parent, after complete link metadata and the parent DAG have been checked.
Unknown links, immutable blocks and merges require native reconstruction.
This premise concerns the native block encoding; it grants no read authority.
Full Goal payloads at assignment frontiers are always reconstructed natively. -/
structure Facts where
  parentCount : Nat
  metadataComplete : Bool
  assignmentWritten : Bool
  deriving Lean.FromJson, Lean.ToJson, Repr

def canReuse (facts : Facts) : Bool :=
  facts.parentCount == 1 && facts.metadataComplete && !facts.assignmentWritten

def project (facts : Facts) (parent native : Version) : Version :=
  if canReuse facts then
    { native with assignment := parent.assignment, status := parent.status }
  else native

theorem incomplete_metadata_reconstructs (facts : Facts)
    (incomplete : facts.metadataComplete = false) : canReuse facts = false := by
  simp [canReuse, incomplete]

theorem changed_assignment_reconstructs (facts : Facts)
    (changed : facts.assignmentWritten = true) : canReuse facts = false := by
  simp [canReuse, changed]

theorem non_single_parent_reconstructs (facts : Facts)
    (different : facts.parentCount ≠ 1) : canReuse facts = false := by
  simp [canReuse, different]

/-- The native adapter checks the encoding premise against actual CID reads:
an unchanged single-parent revision has the same assignment and status.
All physical commits, heights and parent edges remain the native ones. -/
theorem projection_preserves_native_revision (facts : Facts) (parent native : Version)
    (sameAssignment : parent.assignment = native.assignment)
    (sameStatus : parent.status = native.status) : project facts parent native = native := by
  simp [project, sameAssignment, sameStatus]

def evaluate (input : Lean.Json) : Except String Lean.Json := do
  let facts : Array Facts ← Lean.fromJson? input
  pure (Lean.toJson (facts.map canReuse))

end GoalAutomation.HistoryProjection
