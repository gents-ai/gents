import Proofs.Basic
import Mathlib.Data.List.Basic

namespace CanonicalOutput.ReferenceTable

/-- A stored message substitutes table ordinals only for typed payload closing
references. Provider metadata remains opaque, and the stream ordinal is unchanged. -/
structure Reference where
  close : String
  stream : Nat
  deriving DecidableEq, Repr

structure StoredReference where
  slot : Nat
  stream : Nat
  deriving DecidableEq, Repr

def encode (table : List String) (reference : Reference) : StoredReference :=
  ⟨table.idxOf reference.close, reference.stream⟩

def decode (table : List String) (reference : StoredReference) : Option Reference :=
  table[reference.slot]?.map fun close => ⟨close, reference.stream⟩

theorem roundtrip (table : List String) (reference : Reference)
    (present : reference.close ∈ table) :
    decode table (encode table reference) = some reference := by
  simp only [decode, encode]
  rw [List.getElem?_idxOf present]
  rfl

theorem missing_slot_rejected (table : List String) (slot stream : Nat)
    (missing : table.length ≤ slot) : decode table ⟨slot, stream⟩ = none := by
  simp [decode, List.getElem?_eq_none missing]

structure Case where
  name : String
  table : List String
  slot : Nat
  stream : Nat
  expected : Option Reference
  deriving Repr

def caseOf (name : String) (table : List String) (slot stream : Nat) : Case :=
  { name, table, slot, stream, expected := decode table ⟨slot, stream⟩ }

def cases : List Case :=
  [ caseOf "first" ["close-a", "close-b"] 0 0
  , caseOf "second" ["close-a", "close-b"] 1 9
  , caseOf "repeated" ["close-a", "close-a"] 1 7
  , caseOf "missing" ["close-a"] 1 3
  , caseOf "empty" [] 0 0
  , caseOf "opaque_string" ["0", "🦀"] 1 99
  ]

end CanonicalOutput.ReferenceTable
