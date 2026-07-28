import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image

/-
Directory projection model (issue #714, machine index v1).

`AgentDirectoryEntry` is a source-owned projection of one home's agent
principals: one entry per principal, contents a function of the principal's
payload and stamped with the home/source DID.
The reconciler sweeps on source-collection update events, so the
load-bearing property is that a settled state is a WRITE-FREE fixpoint —
otherwise the sweep self-perpetuates. Ownership is partitioned by source DID:
a home may replace or retract its own derived rows, but replicated rows owned
by another home are structurally preserved.

Payload is abstracted to `payload : String` — the Rust row carries
display_name/behaviors/runtime_state/last_seen, all below this model's
abstraction; what the theorems pin is the projection SHAPE (membership,
idempotence, fixpoint, retraction), not field inventory.
-/

namespace DirectoryProjection

structure Principal where
  did : String
  payload : String
  deriving DecidableEq, Repr

structure Entry where
  source : String
  did : String
  payload : String
  deriving DecidableEq, Repr

/-- The projection: one entry per principal, contents a function of it. -/
def project (source : String) (principals : Finset Principal) : Finset Entry :=
  principals.image (fun p => { source, did := p.did, payload := p.payload })

structure DirectoryState where
  source : String
  principals : Finset Principal
  /-- Rows this source owns and may reconcile. -/
  ownedEntries : Finset Entry
  /-- Replicated rows owned by other sources; never mutated by this sweep. -/
  foreignEntries : Finset Entry
  deriving DecidableEq

namespace DirectoryState

/-- Settled: the live entries equal the projection exactly (full-row
equality, not did-membership — a drifted payload must converge). -/
def settled (s : DirectoryState) : Prop :=
  s.ownedEntries = project s.source s.principals

instance (s : DirectoryState) : Decidable s.settled := by
  unfold settled; infer_instance

/-- One sweep: replace only this source's owned partition with the projection. -/
def projectStep (s : DirectoryState) : DirectoryState :=
  { s with ownedEntries := project s.source s.principals }

end DirectoryState

open DirectoryState

/-- Membership characterization: an entry is projected iff a principal
with exactly that did/payload exists. -/
theorem mem_project {source : String} {principals : Finset Principal} {e : Entry} :
    e ∈ project source principals ↔
      ∃ p ∈ principals,
        e = { source, did := p.did, payload := p.payload } := by
  unfold project
  simp [Finset.mem_image, eq_comm]

/-- A sweep settles the state. -/
theorem projectStep_settles (s : DirectoryState) : (projectStep s).settled := by
  unfold settled projectStep
  rfl

/-- Sweeping twice is sweeping once. -/
theorem projectStep_idempotent (s : DirectoryState) :
    projectStep (projectStep s) = projectStep s := by
  unfold projectStep
  rfl

/-- A sweep never mutates rows owned by another source. -/
theorem projectStep_preserves_foreign (s : DirectoryState) :
    (projectStep s).foreignEntries = s.foreignEntries := by
  rfl

/-- A settled state is a write-free fixpoint: the event-driven sweep must
not emit another update once converged. -/
theorem settled_fixpoint {s : DirectoryState} (h : s.settled) :
    projectStep s = s := by
  unfold settled at h
  unfold projectStep
  cases s
  simp_all

/-- Retraction soundness: erasing a principal removes exactly its entry —
any entry still projected comes from a surviving principal. -/
theorem mem_project_erase {source : String} {principals : Finset Principal} {p : Principal}
    {e : Entry} (h : e ∈ project source (principals.erase p)) :
    ∃ q ∈ principals, q ≠ p ∧
      e = { source, did := q.did, payload := q.payload } := by
  rw [mem_project] at h
  obtain ⟨q, hq, he⟩ := h
  exact ⟨q, Finset.mem_of_mem_erase hq, Finset.ne_of_mem_erase hq, he⟩

end DirectoryProjection
