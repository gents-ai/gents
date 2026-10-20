import Proofs.Workspace.PathCapability

namespace Workspace.ChangeReview
open PathCapability

structure Version where
  blob : String
  executable : Bool := false
  deriving DecidableEq, Repr

def versionValid : Option Version → Bool
  | none => true
  | some version => !version.blob.isEmpty

structure File where
  path : String
  content : Option Version
  staged : Option Version
  unsaved : Bool := false
  safeEntry : Bool := true
  deriving DecidableEq, Repr

structure Edit where
  path : String
  before : Option Version
  after : Option Version
  canonical : Bool := true
  deriving DecidableEq, Repr

/-- Ordinary chats use the existing request's admitted filesystem resource; they
need no IsolatedWorkspace. A root is a resource observation, never a principal.
Both scopes retain immutable snapshots and exact paths. Native authorization
still resolves the physical request and its DID through the existing owners. -/
inductive Scope where
  | workspace (binding : Binding)
  | requestResource (nodeDid root : String) (capability : WorkspacePathCapability) (snapshot : String)
  deriving DecidableEq, Repr

def Scope.capability : Scope → WorkspacePathCapability
  | .workspace binding => binding.capability
  | .requestResource _ _ capability _ => capability

def Scope.snapshot : Scope → String
  | .workspace binding => binding.tree
  | .requestResource _ _ _ snapshot => snapshot

def Scope.wellFormed : Scope → Bool
  | .workspace binding => !binding.workspaceId.isEmpty && !binding.owner.isEmpty &&
      !binding.base.isEmpty && !binding.tree.isEmpty
  | .requestResource nodeDid root _ snapshot =>
      !nodeDid.isEmpty && !root.isEmpty && !snapshot.isEmpty

structure Reference where
  scope : Scope
  requestDocId : String
  reviewId : String
  deriving DecidableEq, Repr

structure Review where
  reference : Reference
  beforeSnapshot : String
  edits : List Edit
  deriving DecidableEq, Repr

/-- Authorization, placement and snapshot authenticity come from their existing
native owners. Host refinement must compare the actual postimage and mode at
the mutation boundary, hold the existing mutation locks, and retain recovery
information for partial filesystem failure. An atomic model transition is not
proof that arbitrary host processes cannot race a filesystem operation. -/
structure Evidence where
  expected : Reference
  authorized : Bool
  resourceVerified : Bool
  snapshotsAvailable : Bool
  noActiveWriter : Bool
  receipt : Option Reference := none
  deriving DecidableEq, Repr

def structurallyValid (review : Review) : Bool :=
  !review.reference.requestDocId.isEmpty && !review.reference.reviewId.isEmpty &&
  review.reference.scope.wellFormed && !review.beforeSnapshot.isEmpty &&
  !review.edits.isEmpty && decide (review.edits.map Edit.path).Nodup &&
  match review.reference.scope.capability with
  | .unrestrictedCompatibility => false
  | .exactPaths paths => review.edits.all fun edit =>
      edit.canonical && paths.contains edit.path && versionValid edit.before &&
      versionValid edit.after && edit.before != edit.after

def matchesPostimage (files : List File) (edit : Edit) : Bool :=
  match files.find? (fun file => file.path == edit.path) with
  | none => false
  | some file => file.safeEntry && !file.unsaved && file.content == edit.after

def restoreFile (review : Review) (file : File) : File :=
  match review.edits.find? (fun edit => edit.path == file.path) with
  | none => file
  | some edit => {file with content := edit.before}

def execute (files : List File) (review : Review) (e : Evidence) :
    List File × Disposition :=
  if !e.authorized || !e.resourceVerified || e.expected != review.reference ||
      !structurallyValid review then (files, .denied)
  else if e.receipt == some review.reference then (files, .recovered)
  else if e.receipt.isSome || !e.snapshotsAvailable || !e.noActiveWriter ||
      !decide (files.map File.path).Nodup ||
      !review.edits.all (matchesPostimage files) then (files, .denied)
  else (files.map (restoreFile review), .accepted)

theorem unauthorized_is_inert (files : List File) (review : Review) (e : Evidence)
    (h : e.authorized = false) : execute files review e = (files, .denied) := by
  simp [execute, h]

theorem wrong_reference_is_inert (files : List File) (review : Review) (e : Evidence)
    (h : e.expected ≠ review.reference) : execute files review e = (files, .denied) := by
  simp [execute, h]

theorem denied_is_inert (files : List File) (review : Review) (e : Evidence)
    (h : (execute files review e).2 = .denied) :
    (execute files review e).1 = files := by
  simp only [execute] at h ⊢
  split
  · rfl
  · split
    · rfl
    · split
      · rfl
      · simp_all

theorem recovered_is_inert (files : List File) (review : Review) (e : Evidence)
    (h : (execute files review e).2 = .recovered) :
    (execute files review e).1 = files := by
  simp only [execute] at h ⊢
  split
  · rfl
  · split
    · rfl
    · split
      · rfl
      · simp_all

theorem restore_preserves_index_and_buffer (review : Review) (file : File) :
    (restoreFile review file).staged = file.staged ∧
    (restoreFile review file).unsaved = file.unsaved ∧
    (restoreFile review file).path = file.path ∧
    (restoreFile review file).safeEntry = file.safeEntry := by
  simp only [restoreFile]
  split <;> exact ⟨rfl, rfl, rfl, rfl⟩

theorem unrecorded_file_unchanged (review : Review) (file : File)
    (h : review.edits.find? (fun edit => edit.path == file.path) = none) :
    restoreFile review file = file := by
  simp [restoreFile, h]

theorem restored_files_preserve_index (files : List File) (review : Review) :
    (files.map (restoreFile review)).map (fun file => (file.path, file.staged)) =
      files.map (fun file => (file.path, file.staged)) := by
  simp only [List.map_map, Function.comp_def]
  congr 1
  funext file
  exact Prod.ext (restore_preserves_index_and_buffer review file).2.2.1
    (restore_preserves_index_and_buffer review file).1

theorem execute_preserves_index (files : List File) (review : Review) (e : Evidence) :
    (execute files review e).1.map (fun file => (file.path, file.staged)) =
      files.map (fun file => (file.path, file.staged)) := by
  unfold execute
  split
  · rfl
  · split
    · rfl
    · split
      · rfl
      · exact restored_files_preserve_index files review

theorem postimage_conflict_is_inert (files : List File) (review : Review) (e : Evidence)
    (h : review.edits.all (matchesPostimage files) = false) (noReceipt : e.receipt = none) :
    execute files review e = (files, .denied) := by
  simp [execute, h, noReceipt]

theorem no_active_writer_required_for_fresh_undo (files : List File) (review : Review)
    (e : Evidence) (h : e.noActiveWriter = false) (noReceipt : e.receipt = none) :
    execute files review e = (files, .denied) := by
  simp [execute, h, noReceipt]

theorem unavailable_snapshots_cannot_restore (files : List File) (review : Review)
    (e : Evidence) (h : e.snapshotsAvailable = false) (noReceipt : e.receipt = none) :
    execute files review e = (files, .denied) := by
  simp [execute, h, noReceipt]

theorem missing_request_resource_cannot_restore (files : List File) (review : Review)
    (e : Evidence) (h : e.resourceVerified = false) :
    execute files review e = (files, .denied) := by
  simp [execute, h]

end Workspace.ChangeReview
