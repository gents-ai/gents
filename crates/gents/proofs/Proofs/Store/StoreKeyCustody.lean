/-! Custody of a store's at-rest key: what preparing, resuming and
loading a custody record do, given whether the store has data and whether its
key file exists. -/

namespace StoreKeyCustody

inductive Outcome where
  | prepared
  | created
  | loaded
  | unencryptedStore
  | unrecordedKeyFile
  | missingStoreKey
  | foreignVersion
  deriving DecidableEq, Repr

inductive KeyFile where
  | absent | unchanged | created
  deriving DecidableEq, Repr

structure Step where
  outcome : Outcome
  keyFile : KeyFile
  /-- For a refusal of the store itself: whether an older build wrote it, so
  that starting fresh is offered. A store another version wrote is never
  offered for deletion. -/
  older : Option Bool
  deriving DecidableEq, Repr

def Outcome.older : Outcome → Option Bool
  | .unencryptedStore | .missingStoreKey => some true
  | .foreignVersion => some false
  | _ => none

def unchanged (keyPresent : Bool) (outcome : Outcome) : Step :=
  { outcome, keyFile := if keyPresent then .unchanged else .absent, older := outcome.older }

set_option linter.unusedVariables false in
/-- Allocating custody for a store that has none. External premise: a
Regolith store with data has a `MANIFEST`, which is what `storeData` reads.
A store with data and no record was written unencrypted, and whatever
already sits at the key file's path is someone else's. The custody choice
changes neither refusal. -/
def prepare (keychain storeData keyPresent : Bool) : Step :=
  unchanged keyPresent <|
    if storeData then .unencryptedStore
    else if keyPresent then .unrecordedKeyFile
    else .prepared

def load (currentVersion keyPresent : Bool) : Step :=
  unchanged keyPresent <|
    if !currentVersion then .foreignVersion
    else if keyPresent then .loaded
    else .missingStoreKey

/-- A recorded intent creates its key only while the store has no data: once
data exists, the key that encrypted it is gone and no new key can read it. -/
def resume (currentVersion keyPresent storeData : Bool) : Step :=
  if currentVersion && !keyPresent && !storeData then
    { outcome := .created, keyFile := .created, older := none }
  else load currentVersion keyPresent

theorem an_existing_key_file_is_never_replaced (a b c : Bool) :
    (prepare a b true).keyFile = .unchanged
      ∧ (load a true).keyFile = .unchanged
      ∧ (resume a true b).keyFile = .unchanged
      ∧ (prepare a b c).keyFile ≠ .created ∧ (load a c).keyFile ≠ .created := by
  cases a <;> cases b <;> cases c <;> decide

theorem a_key_is_created_only_for_a_store_without_data
    (currentVersion keyPresent storeData : Bool)
    (h : (resume currentVersion keyPresent storeData).keyFile = .created) :
    currentVersion = true ∧ keyPresent = false ∧ storeData = false := by
  cases currentVersion <;> cases keyPresent <;> cases storeData <;> simp_all [resume, load, unchanged]

theorem a_lost_key_of_a_store_with_data_is_a_refusal :
    (resume true false true).outcome = .missingStoreKey := rfl

theorem the_custody_choice_does_not_change_the_refusal (storeData keyPresent : Bool) :
    prepare true storeData keyPresent = prepare false storeData keyPresent := rfl

theorem a_foreign_version_is_never_offered_for_deletion (keyPresent : Bool) :
    (load false keyPresent).older = some false := by
  cases keyPresent <;> rfl

end StoreKeyCustody
