import Proofs.AgentSession

namespace AgentSession.Archive

/-- Archive is shared canonical session metadata, visible to existing ACP-authorized
readers. The adapter supplies the native DID/ACP authorization fact;
this projection grants neither read nor mutation authority. Archive does not close,
stop, cancel or recreate a session. Its revision fences stale observations, including
an archive/restore cycle that returns to the same visible state. Native adapters
bound revisions to the existing database integer range and preserve timestamps as
validated RFC3339 values; Nat identifiers and times abstract their wire encoding. -/
inductive Decision where
  | denied | unchanged | conflict | exhausted | applied
  deriving DecidableEq, Repr

inductive Visibility where
  | active | archived | all
  deriving DecidableEq, Repr

def visible (selection : Visibility) (s : Document) : Bool :=
  match selection with
  | .active => s.archivedAt.isNone
  | .archived => s.archivedAt.isSome
  | .all => true

def apply (s : Document) (authorized : Bool) (expected : Nat)
    (archived : Bool) (now limit : Nat) : Decision × Document :=
  if !authorized then (.denied, s)
  else if s.archivedAt.isSome = archived then (.unchanged, s)
  else if s.archiveRevision ≠ expected then (.conflict, s)
  else if limit ≤ s.archiveRevision then (.exhausted, s)
  else (.applied, { s with
    archivedAt := if archived then some now else none
    archiveRevision := s.archiveRevision + 1 })

def retained (s : Document) :=
  (s.scope, s.agent, s.createdAt, s.closedAt, s.stoppedAt, s.title, s.tags, s.provenance, s.observation)

theorem denied_preserves_document (s : Document) (expected : Nat)
    (archived : Bool) (now limit : Nat) :
    apply s false expected archived now limit = (.denied, s) := by
  simp [apply]

theorem same_state_preserves_document (s : Document) (expected : Nat)
    (archived : Bool) (now limit : Nat) (h : s.archivedAt.isSome = archived) :
    apply s true expected archived now limit = (.unchanged, s) := by
  simp [apply, h]

theorem stale_change_preserves_document (s : Document) (expected : Nat)
    (archived : Bool) (now limit : Nat) (hstate : s.archivedAt.isSome ≠ archived)
    (hversion : s.archiveRevision ≠ expected) :
    apply s true expected archived now limit = (.conflict, s) := by
  simp [apply, hstate, hversion]

theorem archive_preserves_history (s : Document) (authorized : Bool) (expected : Nat)
    (archived : Bool) (now limit : Nat) :
    retained (apply s authorized expected archived now limit).2 = retained s := by
  unfold apply
  split
  · rfl
  · split
    · rfl
    · split
      · rfl
      · split <;> rfl

theorem applied_advances_revision (s : Document) (now limit : Nat) (archived : Bool)
    (hstate : s.archivedAt.isSome ≠ archived) (hlimit : s.archiveRevision < limit) :
    (apply s true s.archiveRevision archived now limit).2.archiveRevision =
      s.archiveRevision + 1 := by
  simp [apply, hstate, Nat.not_le.mpr hlimit]

theorem active_and_archived_partition (s : Document) :
    visible .active s = !visible .archived s := by
  cases h : s.archivedAt <;> simp [visible, h]

end AgentSession.Archive
