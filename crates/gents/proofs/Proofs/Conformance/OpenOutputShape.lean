import Proofs.Session.CanonicalOutput.OpenShape
import Proofs.Session.CanonicalOutput.TailAppend
import Proofs.Conformance.Family

namespace CanonicalOutput

deriving instance Lean.ToJson for PayloadKind, OpenShape,
  TailAppendKind, TailAppendPlan, TailAppendError

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
  streamBytesAfter : Option (List Nat) := none
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
    createdAt := record.createdAt,
    streamBytesAfter := record.streamBytesAfter }

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
    [{ first with streamBytesAfter := some [3] },
      { second with streamBytesAfter := some [6] }],
    [{ first with streamBytesAfter := some [99] }],
    [{ id := 9, ordinal := none, runs := [], payload := "", close := true,
        streamBytesAfter := some [0] }],
    (List.range 8).map fun ordinal => flush (ordinal + 1) ordinal "chunk"
  ]
  model := fun records =>
    let selected := records.filter fun record => record.request == 1 && record.source == 1
    let segments := records.map RecordInput.segment
    let selectedSegments := sourceRecords segments ⟨1, .tool 1⟩
    let payloadsValid := selected.all payloadValid && selectedSegments.all (exactIdentityAt selectedSegments)
    { full := (inspectValidatedOpenShape segments ⟨1, .tool 1⟩ (.tool 1) payloadsValid).toOption,
      shape := (inspectOpenShape segments ⟨1, .tool 1⟩ (.tool 1)).toOption }

structure TailAppendInput where
  tail : List RecordInput
  closures : List RecordInput := []
  sameAttempt : List RecordInput := []
  writer : Nat := 1
  createdAt : Nat := 10
  payloadBytes : Nat := 4
  deriving Lean.ToJson

structure TailAppendOutput where
  plan : Option TailAppendPlan
  error : Option TailAppendError
  deriving Lean.ToJson

def flushWithBytes (id ordinal totalBytes : Nat) (payload : String) (createdAt : Nat := ordinal) :
    RecordInput :=
  { flush id ordinal payload with
    createdAt := createdAt
    streamBytesAfter := some [totalBytes] }

def tail0 := flushWithBytes 1 0 3 "one" 1
def tail1 := flushWithBytes 2 1 6 "two" 2
def tail2 := flushWithBytes 3 2 11 "three" 3
def tailClose : RecordInput :=
  { id := 99, ordinal := none, runs := [], payload := "", close := true, createdAt := 5 }

@[conformance_family]
def planToolAppend : Family TailAppendInput TailAppendOutput where
  inputs := [
    -- Empty source -> fresh ordinal 0
    { tail := [], createdAt := 1, payloadBytes := 5 },
    -- Single-segment tail (ordinal 0) -> fresh ordinal 1
    { tail := [tail0], createdAt := 2, payloadBytes := 3 },
    -- Multi-segment tail (ordinals 2, 1, 0) -> fresh ordinal 3
    { tail := [tail2, tail1, tail0], createdAt := 4, payloadBytes := 7 },
    -- Equal timestamp is nondecreasing and accepted
    { tail := [tail2, tail1], createdAt := 3, payloadBytes := 4 },
    -- Empty payload rejected
    { tail := [tail0], createdAt := 2, payloadBytes := 0 },
    -- Writer mismatch at request/source boundary
    { tail := [], writer := 2, createdAt := 1, payloadBytes := 4 },
    -- Writer mismatch on observed tail row
    { tail := [{ tail0 with writer := 2 }], createdAt := 2, payloadBytes := 4 },
    -- Closure witness rejects append
    { tail := [tail0], closures := [tailClose], createdAt := 6, payloadBytes := 4 },
    -- Final-flush closure in tail rejects append
    { tail := [{ tail0 with close := true }], createdAt := 2, payloadBytes := 4 },
    -- Conflicting twin at max ordinal rejects append
    { tail := [tail1, { tail1 with id := 9, payload := "alt" }, tail0], createdAt := 5, payloadBytes := 4 },
    -- Conflicting twin at predecessor ordinal (max - 1) rejects append
    { tail := [tail2, tail1, { tail1 with id := 9, payload := "alt" }], createdAt := 5, payloadBytes := 4 },
    -- Same physical id with conflicting facts rejects append
    { tail := [tail1, { tail1 with createdAt := 99 }], createdAt := 100, payloadBytes := 4 },
    -- Exact duplicate of the same physical row is harmless
    { tail := [tail1, tail1, tail0], createdAt := 5, payloadBytes := 4 },
    -- Missing predecessor (ordinal gap before tail) rejects append
    { tail := [tail2], createdAt := 5, payloadBytes := 4 },
    -- Missing streamBytesAfter on tail rejects append
    { tail := [{ tail0 with streamBytesAfter := none }], createdAt := 5, payloadBytes := 4 },
    -- Mismatched cumulative bytes between predecessor and latest rejects append
    { tail := [{ tail1 with streamBytesAfter := some [99] }, tail0], createdAt := 5, payloadBytes := 4 },
    -- Regressed timestamp relative to latest rejects append
    { tail := [tail1, tail0], createdAt := 1, payloadBytes := 4 },
    -- Regressed timestamp between predecessor and latest rejects append
    { tail := [{ tail1 with createdAt := 0 }, tail0], createdAt := 5, payloadBytes := 4 },
    -- Non-closing row with ordinal = none rejects append
    { tail := [{ id := 8, ordinal := none, runs := [], payload := "", close := false }], createdAt := 5, payloadBytes := 4 },
    -- Idempotent retry of ordinal 0 (sole segment)
    { tail := [tail0], sameAttempt := [tail0], createdAt := 1, payloadBytes := 3 },
    -- Idempotent retry of latest segment (ordinal 1, predecessor ordinal 0 in tail)
    { tail := [tail1, tail0], sameAttempt := [tail1], createdAt := 2, payloadBytes := 3 },
    -- Idempotent retry of earlier segment (ordinal 0 while ordinal 1, 2 exist)
    { tail := [tail2, tail1, tail0], sameAttempt := [tail0], createdAt := 1, payloadBytes := 3 },
    -- Idempotent retry of earlier segment (ordinal 1 while ordinal 2 exists)
    { tail := [tail2, tail1], sameAttempt := [tail1], createdAt := 2, payloadBytes := 3 },
    -- Conflicting retry byte length rejected
    { tail := [tail1, tail0], sameAttempt := [tail1], createdAt := 2, payloadBytes := 9 },
    -- Ambiguous retry identity (two distinct rows at same createdAt) rejected
    { tail := [tail1, tail0], sameAttempt := [tail1, { tail1 with id := 99 }], createdAt := 2, payloadBytes := 3 },
    -- Retry when ordinal has a conflicting twin in tail rejected
    { tail := [tail1, { tail1 with id := 99, createdAt := 3 }, tail0], sameAttempt := [tail1], createdAt := 2, payloadBytes := 3 },
    -- Retry when predecessor gap exists at tail rejected
    { tail := [tail1], sameAttempt := [tail1], createdAt := 2, payloadBytes := 3 },
    -- Foreign source rows in tail are ignored
    { tail := [{ tail0 with source := 2 }], createdAt := 2, payloadBytes := 4 }
  ]
  model := fun input =>
    match planToolAppendFromTail
        (input.tail.map RecordInput.segment)
        (input.closures.map RecordInput.segment)
        (input.sameAttempt.map RecordInput.segment)
        ⟨1, .tool 1⟩ (.tool input.writer) input.createdAt input.payloadBytes with
    | .ok plan => { plan := some plan, error := none }
    | .error err => { plan := none, error := some err }

end Conformance.OpenOutputShape
