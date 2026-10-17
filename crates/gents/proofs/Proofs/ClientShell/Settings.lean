import Proofs.SelfConfig.Apply

namespace ClientShell.Settings

/-- Physical document identity and DefraDB composite CID, read in the same
configuration transaction as the retained document. A field commit or a local
projection counter cannot stand in for either reference. -/
structure Revision where
  document : Nat
  composite : Nat
  deriving DecidableEq, Repr

def revisionMatches (expected current : Option Revision) : Bool :=
  match expected, current with
  | some expected, some current => decide (expected = current)
  | _, _ => false

/-- Backend authentication can contain credentials. The editor exposes only
canonical writable fields outside that group; native account and credential
commands keep ownership of authentication. -/
def editableFields (target : SelfConfig.Target) : List String :=
  (SelfConfig.writableFields target).filter
    (fun field => !(target == .inferenceBackend && field == "auth"))

def patchVisible (target : SelfConfig.Target) (patch : SelfConfig.Patch) : Bool :=
  patch.all (fun entry => (editableFields target).contains entry.key)

def prepare (expected current : Option Revision)
    (validate guard : SelfConfig.Doc → Bool) (target : SelfConfig.Target)
    (stored : SelfConfig.Doc) (patch : SelfConfig.Patch) : Option SelfConfig.Doc :=
  if revisionMatches expected current && patchVisible target patch then
    SelfConfig.step validate guard target stored patch
  else none

/-- A successful save is already committed. Adopt a subsequent read as the
next edit baseline only when it retains the physical document and exactly the
approved visible configuration. Otherwise the editor must reopen a fresh
document before another save; observation failure never revokes the receipt. -/
def adoptSavedProjection (readSucceeded physicalMatches contentMatches : Bool) : Bool :=
  readSucceeded && physicalMatches && contentMatches

theorem missing_revision_refused (current : Option Revision) :
    revisionMatches none current = false := by cases current <;> rfl

theorem changed_revision_refused (expected current : Revision) (h : expected ≠ current) :
    revisionMatches (some expected) (some current) = false := by
  simp [revisionMatches, h]

theorem backend_auth_is_not_editable :
    "auth" ∉ editableFields .inferenceBackend := by decide

theorem editable_fields_are_canonical (target : SelfConfig.Target) (field : String)
    (h : field ∈ editableFields target) : field ∈ SelfConfig.writableFields target := by
  exact (List.mem_filter.mp h).1

theorem failed_read_cannot_advance_baseline (physical content : Bool) :
    adoptSavedProjection false physical content = false := rfl

theorem unseen_public_change_cannot_advance_baseline (read physical : Bool) :
    adoptSavedProjection read physical false = false := by
  simp [adoptSavedProjection]

end ClientShell.Settings
