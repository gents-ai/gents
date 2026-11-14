/-! The metadata file that records a home's client identity (`principal.json`)
beside its signing key. Two first-run processes may share one home (a `gents`
invocation beside the desktop app, or two invocations), so both may find the
metadata absent and both install it. The interleaving of their file
operations is `tla/ClientIdentityMetadata.tla`; this module is one writer's
choice at each of its two reads (`Read` and `Decide` there). An unreadable or
unparsable file fails the read itself, before any choice. -/

namespace ClientIdentityMetadata

/-- The metadata a writer finds, relative to the metadata of the key it
holds. -/
inductive Stored where
  | absent
  | equal
  | different
  deriving DecidableEq, Repr

/-- Which of its two reads the writer is at. -/
inductive Read where
  | first
  /-- After the writer renamed its own staging file over the destination. -/
  | afterInstall
  deriving DecidableEq, Repr

inductive Decision where
  | accept
  /-- Stage the metadata under a name no other writer can pick and rename it
  over the destination, without removing the destination first. -/
  | install
  /-- The hard error: the home records another identity. -/
  | mismatch
  /-- The hard error: the destination is gone after this writer installed it. -/
  | missing
  deriving DecidableEq, Repr

/-- A writer that lost the race to install re-reads and accepts metadata equal
to its own, because the signing key is single-winner and every writer of the
home therefore describes the same key (`SameKeyAccepts`). The second read
never installs again: a destination absent after the writer's own rename means
something removed an installed identity, which no writer does
(`NeverUninstalled`; the TLC counterexample with a removal before the rename
is `RemoveThenRenameReread`), so it is reported instead of accepted. -/
def decide : Read → Stored → Decision
  | _, .equal => .accept
  | _, .different => .mismatch
  | .first, .absent => .install
  | .afterInstall, .absent => .missing

theorem only_equal_metadata_is_accepted (read : Read) (stored : Stored)
    (h : decide read stored = .accept) : stored = .equal := by
  cases read <;> cases stored <;> simp_all [decide]

theorem a_lost_race_accepts_equal_metadata : decide .afterInstall .equal = .accept := rfl

theorem different_metadata_is_always_a_mismatch (read : Read) :
    decide read .different = .mismatch := by
  cases read <;> rfl

theorem only_absent_metadata_is_installed (read : Read) (stored : Stored)
    (h : decide read stored = .install) : read = .first ∧ stored = .absent := by
  cases read <;> cases stored <;> simp_all [decide]

/-- A writer installs at most once: its second read ends in acceptance or a
hard error. -/
theorem the_second_read_never_installs (stored : Stored) :
    decide .afterInstall stored ≠ .install := by
  cases stored <;> simp [decide]

theorem a_missing_file_is_reported_only_after_an_install (read : Read) (stored : Stored)
    (h : decide read stored = .missing) : read = .afterInstall ∧ stored = .absent := by
  cases read <;> cases stored <;> simp_all [decide]

end ClientIdentityMetadata
