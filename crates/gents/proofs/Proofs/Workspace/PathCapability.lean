import Proofs.Workspace.Properties

/-! Host operation refinement of the existing workspace/receipt/journal owners.
Canonicalization and exact Git delta capture are observed host predicates; this
model does not prove OS path alias, filesystem isolation or hash collision facts.
The coarse Workspace lifecycle is necessary but not sufficient for host effects.
-/
namespace Workspace.PathCapability
open WorkspacePathCapability

inductive EntryKind where
  | regular | symlink | gitlink
  deriving DecidableEq, Repr
structure Change where
  path : String
  kind : EntryKind := .regular
  canonical : Bool := true
  deriving DecidableEq, Repr

def deltaAuthorized (cap : WorkspacePathCapability) (delta : List Change) : Bool :=
  match cap with
  | .unrestrictedCompatibility => true
  | .exactPaths paths => delta.all (fun c => c.canonical && c.kind == .regular && paths.contains c.path)

/-- Source-version classification is supplied by migration, never model input. -/
def migrateCapability (legacySource : Bool) (stored : Option WorkspacePathCapability) :
    Option WorkspacePathCapability :=
  if legacySource then some .unrestrictedCompatibility else stored

def freshAdmitted (cap : WorkspacePathCapability) (canonical : Bool) : Bool :=
  match cap with | .exactPaths _ => canonical | .unrestrictedCompatibility => false

structure Binding where
  workspaceId : String
  owner : String
  base : String
  capability : WorkspacePathCapability
  tree : String
  deriving DecidableEq, Repr

def binding (w : IsolatedWorkspace) (tree : String) : Binding :=
  ⟨w.workspaceId, w.ownerDeploymentId, w.baseSha, w.pathCapability, tree⟩

inductive Operation where
  | provision | seal | integrate | replaySeal | replayIntegrate
  deriving DecidableEq, Repr
structure Snapshot where
  workspace : IsolatedWorkspace
  writer : Option Binding
  integrator : Option Binding
  trunkEffects : Nat
  deriving DecidableEq, Repr
structure Evidence where
  expected : Binding
  delta : List Change
  manifestCanonical : Bool := true
  checkoutPresent : Bool := true
  existingIdentityMatches : Bool := false
  capturedBaseMatches : Bool := true
  appliedSnapshotMatches : Bool := true
  deriving DecidableEq, Repr
inductive Disposition where
  | accepted | recovered | denied
  deriving DecidableEq, Repr

def execute (s : Snapshot) (op : Operation) (e : Evidence) : Snapshot × Disposition :=
  let w := s.workspace
  if e.expected != binding w e.expected.tree then (s, .denied)
  else match op with
  | .replaySeal =>
      if s.writer == some e.expected then (s, .recovered) else (s, .denied)
  | .replayIntegrate =>
      if s.integrator == some e.expected then (s, .recovered) else (s, .denied)
  | .provision =>
      if e.existingIdentityMatches && e.checkoutPresent then (s, .recovered)
      else if w.state == .provisioning && freshAdmitted w.pathCapability e.manifestCanonical then
        ({s with workspace := {w with state := .ready}}, .accepted)
      else (s, .denied)
  | .seal | .integrate =>
      if !e.checkoutPresent || !e.manifestCanonical || !e.capturedBaseMatches ||
          !e.appliedSnapshotMatches || e.expected.tree.isEmpty ||
          !deltaAuthorized w.pathCapability e.delta then (s, .denied)
      else if op == .seal then
        if w.state == .ready || (w.state == .sealed && w.sealHash == some e.expected.tree) then
          ({s with workspace := {w with state := .sealed, sealHash := some e.expected.tree},
                   writer := some e.expected}, .accepted)
        else (s, .denied)
      else if w.state == .sealed && w.sealHash == some e.expected.tree && s.writer == some e.expected then
        if s.integrator == some e.expected then (s, .recovered)
        else ({s with integrator := some e.expected, trunkEffects := s.trunkEffects + 1}, .accepted)
      else (s, .denied)

theorem fresh_legacy_never_admitted (canonical : Bool) :
    freshAdmitted .unrestrictedCompatibility canonical = false := rfl

theorem missing_new_capability_not_migrated : migrateCapability false none = none := rfl

theorem legacy_migration_explicit :
    migrateCapability true none = some .unrestrictedCompatibility := rfl

/-- The predecessor schema has no capability field; injected values cannot
be mistaken for an admitted exact grant during migration. -/
theorem legacy_injected_capability_overwritten (injected : WorkspacePathCapability) :
    migrateCapability true (some injected) = some .unrestrictedCompatibility := rfl

theorem empty_exact_denies_changed_regular_path (path : String) :
    deltaAuthorized (.exactPaths []) [⟨path,.regular,true⟩] = false := by
  simp [deltaAuthorized]

theorem capability_preserved (s : Snapshot) (op : Operation) (e : Evidence) :
    (execute s op e).1.workspace.pathCapability = s.workspace.pathCapability := by
  cases op <;> simp only [execute]
  all_goals repeat first | split | rfl

theorem exact_receipt_replay_no_effect (s : Snapshot) (e : Evidence)
    (h : e.expected = binding s.workspace e.expected.tree)
    (receipt : s.integrator = some e.expected) :
    execute s .replayIntegrate e = (s,.recovered) := by
  have guard : (e.expected != binding s.workspace e.expected.tree) = false := by
    have heq : (e.expected == binding s.workspace e.expected.tree) = true := beq_iff_eq.mpr h
    simp only [bne, heq, Bool.not_true]
  simp only [execute, guard, Bool.false_eq_true, ↓reduceIte, receipt, beq_self_eq_true]

theorem denied_changes_nothing (s : Snapshot) (op : Operation) (e : Evidence)
    (h : (execute s op e).2 = .denied) : (execute s op e).1 = s := by
  cases op <;> simp only [execute] at *
  all_goals repeat first | split at * | simp_all

theorem unauthorized_delta_cannot_seal (s : Snapshot) (e : Evidence)
    (h : deltaAuthorized s.workspace.pathCapability e.delta = false) :
    execute s .seal e = (s,.denied) := by
  simp [execute, h]

theorem unauthorized_delta_cannot_integrate (s : Snapshot) (e : Evidence)
    (h : deltaAuthorized s.workspace.pathCapability e.delta = false) :
    execute s .integrate e = (s,.denied) := by
  simp [execute, h]

end Workspace.PathCapability
