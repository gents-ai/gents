import Proofs.Workspace.PathCapability

/-! Narrow operator operation in the existing Workspace executor. It does not
relax writer sealing or integration and creates no producer request identity.
Host capability/placement verification, exact Git base-tree observation and
active-binding query are adapter evidence, not filesystem or isolation proofs.
-/
namespace Workspace.PathCapability.OperatorBaseFreeze

structure Evidence where
  expected : Binding
  baseTree : String
  sealCapability : Bool
  ownerAndPlacementVerified : Bool
  noActiveWriter : Bool
  checkoutPresent : Bool
  manifestCanonical : Bool
  capturedBaseMatches : Bool
  delta : List Change
  deriving DecidableEq, Repr

/-- Exact persisted seal recovery does not need a deleted checkout. Both new
freeze and replay remain authorized by the existing operator capability. -/
def execute (s : Snapshot) (e : Evidence) : Snapshot × Disposition :=
  let w := s.workspace
  if !e.sealCapability || !e.ownerAndPlacementVerified ||
      e.expected != binding w e.baseTree || e.baseTree.isEmpty ||
      w.pathCapability != .exactPaths [] then (s,.denied)
  else if w.state == .sealed && w.sealHash == some e.baseTree then (s,.recovered)
  else if w.state == .ready && e.noActiveWriter && e.checkoutPresent &&
      e.manifestCanonical && e.capturedBaseMatches && e.delta.isEmpty then
    ({s with workspace := {w with state := .sealed, sealHash := some e.baseTree}}, .accepted)
  else (s,.denied)

theorem receipts_preserved (s : Snapshot) (e : Evidence) :
    (execute s e).1.writer = s.writer ∧
    (execute s e).1.integrator = s.integrator ∧
    (execute s e).1.trunkEffects = s.trunkEffects := by
  simp only [execute]
  repeat first | split | exact ⟨rfl,rfl,rfl⟩

theorem path_capability_preserved (s : Snapshot) (e : Evidence) :
    (execute s e).1.workspace.pathCapability = s.workspace.pathCapability := by
  simp only [execute]
  repeat first | split | rfl

theorem no_operator_capability_denied (s : Snapshot) (e : Evidence)
    (h : e.sealCapability = false) : execute s e = (s,.denied) := by
  simp [execute,h]

theorem changed_delta_cannot_freeze_ready (s : Snapshot) (e : Evidence)
    (hs : s.workspace.state = .ready) (hd : e.delta.isEmpty = false) :
    execute s e = (s,.denied) := by
  simp [execute,hs,hd]

theorem active_writer_cannot_freeze_ready (s : Snapshot) (e : Evidence)
    (hs : s.workspace.state = .ready) (hw : e.noActiveWriter = false) :
    execute s e = (s,.denied) := by
  simp [execute,hs,hw]

end Workspace.PathCapability.OperatorBaseFreeze
