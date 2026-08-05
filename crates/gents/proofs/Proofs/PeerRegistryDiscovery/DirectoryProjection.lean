import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image

namespace DirectoryProjection

/-- The projected row's contents are abstracted as an opaque `payload`
string: display name, behavior name/id arrays, runtime state — and, since
the persona catalog (#986), the per-behavior dimension arrays
(models/roots/presets/profiles) and home-level composer option lists. All
of these ride below this abstraction; only the (source, did) identity and
payload-equality drive the projection theorems. -/
structure Principal where
  did : String
  payload : String
  deriving DecidableEq, Repr

def directoryKey (source did : String) : String := source ++ "\x1f" ++ did

structure Entry where
  key : String
  source : String
  did : String
  payload : String
  deriving DecidableEq, Repr

def project (source : String) (principals : Finset Principal) : Finset Entry :=
  principals.image (fun p => {
    key := directoryKey source p.did, source, did := p.did, payload := p.payload })

structure DirectoryState where
  source : String
  principals : Finset Principal
  ownedEntries : Finset Entry
  foreignEntries : Finset Entry
  deriving DecidableEq

namespace DirectoryState

def settled (s : DirectoryState) : Prop :=
  s.ownedEntries = project s.source s.principals

instance (s : DirectoryState) : Decidable s.settled := by
  unfold settled; infer_instance

def projectStep (s : DirectoryState) : DirectoryState :=
  { s with ownedEntries := project s.source s.principals }

end DirectoryState

open DirectoryState

theorem mem_project {source : String} {principals : Finset Principal} {e : Entry} :
    e ∈ project source principals ↔
      ∃ p ∈ principals,
        e = { key := directoryKey source p.did, source, did := p.did, payload := p.payload } := by
  unfold project
  simp [Finset.mem_image, eq_comm]

theorem projectStep_settles (s : DirectoryState) : (projectStep s).settled := by
  unfold settled projectStep
  rfl

theorem projectStep_idempotent (s : DirectoryState) :
    projectStep (projectStep s) = projectStep s := by
  unfold projectStep
  rfl

theorem projectStep_preserves_foreign (s : DirectoryState) :
    (projectStep s).foreignEntries = s.foreignEntries := by
  rfl

theorem projectStep_preserves_foreign_same_did (s : DirectoryState) {foreign : Entry}
    (h : foreign ∈ s.foreignEntries) : foreign ∈ (projectStep s).foreignEntries := by
  simpa [projectStep] using h

theorem settled_fixpoint {s : DirectoryState} (h : s.settled) :
    projectStep s = s := by
  unfold settled at h
  unfold projectStep
  cases s
  simp_all

theorem mem_project_erase {source : String} {principals : Finset Principal} {p : Principal}
    {e : Entry} (h : e ∈ project source (principals.erase p)) :
    ∃ q ∈ principals, q ≠ p ∧
      e = { key := directoryKey source q.did, source, did := q.did, payload := q.payload } := by
  rw [mem_project] at h
  obtain ⟨q, hq, he⟩ := h
  exact ⟨q, Finset.mem_of_mem_erase hq, Finset.ne_of_mem_erase hq, he⟩

end DirectoryProjection
