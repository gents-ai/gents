import Proofs.Session.CanonicalOutput.OpenShape
import Proofs.Conformance.Family

namespace CanonicalOutput

deriving instance Lean.ToJson for PayloadKind, OpenShape

end CanonicalOutput

namespace Conformance.OpenOutputShape

open CanonicalOutput

structure DeclarationInput where
  block : Nat := 0
  part : Nat := 0
  kind : PayloadKind := .toolOutput
  deriving Lean.ToJson

structure RunInput where
  stream : Nat := 0
  bytes : Nat
  declaration : Option DeclarationInput := none
  deriving Lean.ToJson

structure RecordInput where
  id : Nat
  request : Nat := 1
  source : Nat := 1
  writer : Nat := 1
  ordinal : Option Nat
  runs : List RunInput
  payload : String
  close : Bool := false
  createdAt : Nat := 0
  deriving Lean.ToJson

def RecordInput.segment (record : RecordInput) : Segment :=
  { id := record.id, coordinate := ⟨record.request, .tool record.source⟩,
    writer := .tool record.writer,
    flush := record.ordinal.map fun ordinal =>
      { ordinal, runs := record.runs.map fun run =>
          ⟨run.stream, run.bytes, run.declaration.map fun declaration =>
            ⟨declaration.block, declaration.part, declaration.kind, none, none⟩⟩,
        payload := record.payload.toUTF8.data.toList },
    close := if record.close then some (.closed .complete 0 []) else none,
    createdAt := record.createdAt }

def validPartition : List RunInput → List UInt8 → Bool
  | [], bytes => bytes.isEmpty
  | run :: rest, bytes =>
      run.bytes ≤ bytes.length &&
      (String.fromUTF8? (ByteArray.mk (bytes.take run.bytes).toArray)).isSome &&
      validPartition rest (bytes.drop run.bytes)

def payloadValid (record : RecordInput) : Bool :=
  match record.ordinal with
  | none => record.payload.isEmpty
  | some _ => validPartition record.runs record.payload.toUTF8.data.toList

structure Output where
  full : Option OpenShape
  shape : Option OpenShape
  deriving Lean.ToJson

def flush (id ordinal : Nat) (payload : String) : RecordInput :=
  { id, ordinal := some ordinal, payload, createdAt := ordinal,
    runs := [⟨0, payload.toUTF8.size, if ordinal = 0 then some {} else none⟩] }

def first := flush 1 0 "one"
def second := flush 2 1 "two"
def third := flush 3 2 "three"

@[conformance_family]
def inspect : Family (List RecordInput) Output where
  inputs := [
    [], [first], [first, second, third], [first, third],
    [first, { second with ordinal := some 0 }],
    [second, { second with id := 3 }], [first, first],
    [first, { second with id := 1 }], [first, { first with payload := "two" }],
    [{ first with createdAt := 9 }, second],
    [{ first with request := 2 }], [{ first with source := 2 }],
    [{ first with writer := 2 }],
    [first, { id := 9, ordinal := none, runs := [], payload := "", close := true }],
    [{ id := 9, ordinal := none, runs := [], payload := "" }],
    [{ first with runs := [], payload := "" }],
    [first, { second with runs := [⟨0, 0, none⟩], payload := "" }],
    [{ first with runs := [⟨0, 3, none⟩] }],
    [{ first with runs := [⟨0, 1, some ⟨0, 0, .reasoning⟩⟩,
      ⟨1, 1, some ⟨0, 1, .text⟩⟩], payload := "RT" }],
    [{ first with runs := [⟨0, 1, some ⟨0, 0, .text⟩⟩,
      ⟨1, 1, some ⟨0, 0, .text⟩⟩], payload := "AB" }],
    [{ first with runs := [⟨0, 1, some ⟨0, 0, .reasoning⟩⟩,
      ⟨1, 0, some ⟨0, 0, .reasoningStop⟩⟩], payload := "R" },
      { second with runs := [⟨0, 1, none⟩], payload := "x" }],
    [{ first with runs := [⟨0, 2, some {}⟩], payload := "abc" }],
    [{ first with runs := [⟨0, 9, some {}⟩], payload := "abc" }],
    [{ first with runs := [⟨0, 1, some {}⟩, ⟨0, 1, none⟩], payload := "é" }],
    [{ id := 9, ordinal := none, runs := [], payload := "bytes", close := true }],
    (List.range 8).map fun ordinal => flush (ordinal + 1) ordinal "chunk"
  ]
  model := fun records =>
    let selected := records.filter fun record => record.request == 1 && record.source == 1
    let segments := records.map RecordInput.segment
    let selectedSegments := sourceRecords segments ⟨1, .tool 1⟩
    let payloadsValid := selected.all payloadValid && selectedSegments.all (exactIdentityAt selectedSegments)
    { full := (inspectValidatedOpenShape segments ⟨1, .tool 1⟩ (.tool 1) payloadsValid).toOption,
      shape := (inspectOpenShape segments ⟨1, .tool 1⟩ (.tool 1)).toOption }

end Conformance.OpenOutputShape
