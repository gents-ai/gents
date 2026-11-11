/-! Retiring a home's runtime state: which top-level entries retirement owns,
and what an archive or a delete does to them. -/

namespace HomeRetirement

/-- What a top-level entry of a home is to the runtime. `foreign` is anything
the runtime did not put there: the user's files, a nested home, a backup. -/
inductive Role where
  | inventory | lock | maintenance | foreign
  deriving DecidableEq, Repr

/-- Whether something outside the entry still needs it: another home's init
record names a path inside it, or the caller excludes such a path. -/
inductive Reference where
  | unreferenced | otherHome | exclusion
  deriving DecidableEq, Repr

/-- What retirement finds at an entry it listed earlier. `vanished` left
between listing and retirement, and `uninspectable` cannot be examined. A
`link` points at a directory outside the home. -/
inductive Presence where
  | file | directory | link | vanished | uninspectable
  deriving DecidableEq, Repr

inductive Disposition where
  | archive | archiveOntoBackup | delete
  deriving DecidableEq, Repr

structure Item where
  role : Role
  reference : Reference
  presence : Presence
  deriving DecidableEq, Repr

/-- Retirement owns an entry only when the runtime inventory names it and
nothing else points into it. A held lock file is never owned: replacing it
would let another writer take a different lock for the same resource. -/
def Item.owned (item : Item) : Bool :=
  item.role == .inventory && item.reference == .unreferenced

def Item.retirable (item : Item) : Bool :=
  item.owned && item.presence != .vanished && item.presence != .uninspectable

def Item.blocks (item : Item) : Bool :=
  item.owned && item.presence == .uninspectable

inductive Outcome where
  | retired | rolledBack | refused | stopped
  deriving DecidableEq, Repr

/-- A delete removes entries in order and stops at the first it cannot
examine; `stopped` carries that it already has. -/
def deleteWalk : List Item → Bool → List Bool
  | [], _ => []
  | item :: rest, stopped =>
      (!stopped && item.retirable) :: deleteWalk rest (stopped || item.blocks)

/-- An archive moves every owned entry or none: one entry it cannot examine
moves the others back. -/
def archiveWalk (items : List Item) : List Bool :=
  if items.any Item.blocks then items.map fun _ => false else items.map Item.retirable

structure Retirement where
  outcome : Outcome
  /-- Per entry: listed as owned. -/
  owned : List Bool
  /-- Per entry: named in the account of what was moved or removed. -/
  reported : List Bool
  /-- Per entry: still in the home afterwards. -/
  inHome : List Bool
  /-- Per entry: found under the backup afterwards. -/
  archived : List Bool
  backup : Bool
  /-- The permission bits of a backup this retirement created. -/
  backupMode : Option Nat
  /-- What a retired link pointed at is untouched. -/
  outsideIntact : Bool
  deriving DecidableEq, Repr

def remaining (items : List Item) (reported : List Bool) : List Bool :=
  (items.zip reported).map fun (item, gone) => item.presence != .vanished && !gone

def retire (disposition : Disposition) (items : List Item) : Retirement :=
  let owned := items.map Item.owned
  let blocked := items.any Item.blocks
  let none := items.map fun _ => false
  match disposition with
  | .archiveOntoBackup =>
      { outcome := .refused, owned, reported := none, inHome := remaining items none
        archived := none, backup := true, backupMode := Option.none, outsideIntact := true }
  | .archive =>
      let reported := archiveWalk items
      { outcome := if blocked then .rolledBack else .retired
        owned, reported, inHome := remaining items reported, archived := reported
        backup := !blocked, backupMode := if blocked then Option.none else some 0o700
        outsideIntact := true }
  | .delete =>
      let reported := deleteWalk items false
      { outcome := if blocked then .stopped else .retired
        owned, reported, inHome := remaining items reported, archived := none
        backup := false, backupMode := Option.none, outsideIntact := true }

theorem only_an_unreferenced_inventory_entry_is_owned (item : Item) (h : item.owned = true) :
    item.role = .inventory ∧ item.reference = .unreferenced := by
  simpa [Item.owned] using h

theorem a_lock_is_never_owned (reference : Reference) (presence : Presence) :
    ({ role := .lock, reference, presence } : Item).owned = false := rfl

theorem deleteWalk_removes_only_owned_entries :
    ∀ (items : List Item) (stopped : Bool) (index : Nat),
      (deleteWalk items stopped)[index]? = some true →
      (items[index]?).map Item.owned = some true := by
  intro items
  induction items with
  | nil => intro stopped index h; simp [deleteWalk] at h
  | cons item rest ih =>
    intro stopped index h
    cases index with
    | zero =>
      simp [deleteWalk, Item.retirable] at h
      simp [h.2.1.1]
    | succ index => simpa [deleteWalk] using ih _ index (by simpa [deleteWalk] using h)

theorem deleteWalk_removes_nothing_once_stopped :
    ∀ (items : List Item), deleteWalk items true = items.map fun _ => false := by
  intro items
  induction items with
  | nil => rfl
  | cons item rest ih => simp [deleteWalk, ih]

theorem archive_is_all_or_nothing (items : List Item) :
    archiveWalk items = items.map Item.retirable
      ∨ archiveWalk items = items.map fun _ => false := by
  unfold archiveWalk; split <;> simp

theorem a_rolled_back_archive_moves_nothing (items : List Item)
    (h : (retire .archive items).outcome = .rolledBack) :
    (retire .archive items).reported = items.map fun _ => false := by
  simp only [retire] at h ⊢
  split at h
  · next blocked => simp [archiveWalk, blocked]
  · cases h

theorem an_existing_backup_is_never_merged_into (items : List Item) :
    (retire .archiveOntoBackup items).outcome = .refused
      ∧ (retire .archiveOntoBackup items).reported = items.map fun _ => false := ⟨rfl, rfl⟩

end HomeRetirement
