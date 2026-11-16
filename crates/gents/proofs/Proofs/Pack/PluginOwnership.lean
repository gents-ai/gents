import Mathlib.Data.List.Basic
import Mathlib.Data.List.Dedup

/-!
# Who owns an installed plugin

A node holds one installed-plugin record per plugin name. A pack that ships a
plugin owns the record it wrote, by its coordinate. A record written before
packs recorded ownership has no owner and belongs to whoever holds the same
artifact. Installing and removing a pack both ask the record first, so one
pack can never overwrite or remove another pack's plugin.

A record that cannot be read is not an absent record: nothing is installed
over it and nothing removes it.

External premise, with tests in `gents-pack` as evidence: a record is written
by staging a file and renaming it into place, so a reader sees a whole record
or none, and removal checks the owner and deletes under one store lock.
-/

namespace Pack.PluginOwnership

/-- The record a pack finds under the plugin name it claims. -/
inductive Record
  | absent
  | unreadable
  | ownedByClaimant
  | ownedByAnother
  | unownedSameArtifact
  | unownedOtherArtifact
  deriving DecidableEq, Repr

inductive Admission | allowed | refused | unreadable
  deriving DecidableEq, Repr

/-- May the claiming pack install its plugin under this name? -/
def mayInstall : Record → Admission
  | .unreadable => .unreadable
  | .ownedByAnother => .refused
  | _ => .allowed

/-- Does the claiming pack own the record? -/
def owns : Record → Bool
  | .ownedByClaimant | .unownedSameArtifact => true
  | _ => false

inductive Removal | removed | kept | unreadable
  deriving DecidableEq, Repr

/-- The claiming pack removes the record only if it owns it. -/
def removeIfOwned (record : Record) : Removal :=
  if record = .unreadable then .unreadable
  else if owns record then .removed
  else .kept

/-- Is there still something under the name after the claiming pack's removal? -/
def remainsAfterRemoval (record : Record) : Bool :=
  record ≠ .absent && removeIfOwned record ≠ .removed

structure Verdict where
  admission : Admission
  owns : Bool
  removal : Removal
  remains : Bool
  deriving DecidableEq, Repr

/-- Everything the claiming pack learns from, and does to, the record. -/
def verdict (record : Record) : Verdict :=
  { admission := mayInstall record
    owns := owns record
    removal := removeIfOwned record
    remains := remainsAfterRemoval record }

theorem removal_is_the_owners (record : Record) :
    removeIfOwned record = .removed ↔ owns record = true := by
  cases record <;> simp [removeIfOwned, owns]

theorem owner_may_install_again (record : Record) (h : owns record = true) :
    mayInstall record = .allowed := by
  cases record <;> simp_all [owns, mayInstall]

theorem refusal_names_another_owner (record : Record) :
    mayInstall record = .refused ↔ record = .ownedByAnother := by
  cases record <;> simp [mayInstall]

/-- An unreadable record is never treated as an absent one. -/
theorem unreadable_is_not_absent :
    mayInstall .unreadable = .unreadable ∧ removeIfOwned .unreadable = .unreadable
      ∧ remainsAfterRemoval .unreadable = true := by
  decide

/-- Removal by a pack that does not own the record leaves it in place. -/
theorem others_removal_changes_nothing (record : Record) (hpresent : record ≠ .absent)
    (h : owns record = false) : remainsAfterRemoval record = true := by
  cases record <;> simp_all [owns, remainsAfterRemoval, removeIfOwned]

/-! ## Releasing artifact bytes

Artifact bytes are stored by digest and shared by every record that names the
digest. -/

/-- The candidates freed: those stored and named by no record. -/
def released (referenced stored candidates : List String) : List String :=
  ((candidates.filter fun digest => digest ∈ stored && digest ∉ referenced).dedup).mergeSort
    fun a b => a ≤ b

def remaining (referenced stored candidates : List String) : List String :=
  stored.filter (· ∉ released referenced stored candidates)

theorem mem_released (referenced stored candidates : List String) (digest : String) :
    digest ∈ released referenced stored candidates
      ↔ digest ∈ candidates ∧ digest ∈ stored ∧ digest ∉ referenced := by
  simp [released, List.mem_mergeSort, List.mem_dedup, List.mem_filter]

/-- Bytes a record still names are never released. -/
theorem referenced_bytes_stay (referenced stored candidates : List String) (digest : String)
    (hstored : digest ∈ stored) (h : digest ∈ referenced) :
    digest ∈ remaining referenced stored candidates := by
  simp [remaining, List.mem_filter, hstored, mem_released, h]

/-- Only bytes named as candidates are released. -/
theorem only_candidates_are_released (referenced stored candidates : List String) (digest : String)
    (hstored : digest ∈ stored) (h : digest ∉ candidates) :
    digest ∈ remaining referenced stored candidates := by
  simp [remaining, List.mem_filter, hstored, mem_released, h]

end Pack.PluginOwnership
