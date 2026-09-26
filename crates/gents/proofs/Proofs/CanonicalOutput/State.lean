import Proofs.Basic
import Std.Tactic

/-!
# Immutable output facts (#1571)

Document identities, source coordinates and writer generations are opaque Nat
labels in this model: no theorem derives a DefraDB identity from payload bytes.
`records` is an authorized local observation, not a complete global replica.
Genesis identity, ACP, UTF-8 decoding and transaction atomicity require native
conformance. Payload bytes are modeled explicitly; native decoding is separate.
-/
namespace CanonicalOutput

abbrev DocId := Nat

inductive AuxiliaryKind where
  | compaction
  | compactionFallback
  | title
  deriving DecidableEq, Repr

inductive Source where
  | provider (scope turn attempt : Nat)
  | auxiliary (kind : AuxiliaryKind) (scope turn attempt : Nat)
  | tool (call : DocId)
  | authored (key : Nat)
  deriving DecidableEq, Repr

def Source.isAuxiliary : Source → Bool
  | .auxiliary _ _ _ _ => true
  | _ => false

structure Coordinate where
  request : DocId
  source : Source
  deriving DecidableEq, Repr

inductive Writer where
  | request (generation : Nat)
  | tool (call : DocId)
  deriving DecidableEq, Repr

inductive Outcome where
  | complete
  | «partial»
  deriving DecidableEq, Repr

inductive PayloadKind where
  | text
  | reasoning
  | signature
  | summary
  | encrypted
  | redacted
  | arguments
  | toolOutput
  | media
  deriving DecidableEq, Repr

structure ToolIdentity where
  id : String
  callId : Option String
  name : String
  deriving DecidableEq, Repr

inductive MediaKind where
  | image | audio | video | document
  deriving DecidableEq, Repr

structure Declaration where
  block : Nat
  part : Nat
  kind : PayloadKind
  tool : Option ToolIdentity := none
  mediaKind : Option MediaKind := none
  deriving DecidableEq, Repr

def declarationWellFormed (declaration : Declaration) : Bool :=
  match declaration.kind with
  | .arguments => declaration.tool.isSome && declaration.mediaKind.isNone
  | .media => declaration.tool.isNone && declaration.mediaKind.isSome
  | _ => declaration.tool.isNone && declaration.mediaKind.isNone

structure Run where
  stream : Nat
  bytes : Nat
  declaration : Option Declaration
  deriving DecidableEq, Repr

structure Flush where
  ordinal : Nat
  runs : List Run
  payload : List UInt8
  deriving DecidableEq, Repr

inductive Closure where
  | closed (outcome : Outcome) (segments : Nat) (streamBytes : List Nat)
  | retracted
  deriving DecidableEq, Repr

structure Segment where
  id : DocId
  coordinate : Coordinate
  writer : Writer
  flush : Option Flush
  close : Option Closure
  createdAt : Time
  deriving DecidableEq, Repr

structure PayloadRef where
  closeId : DocId
  stream : Nat
  deriving DecidableEq, Repr

/-- Delivery deduplicates exact facts. A distinct document at the same source
coordinate remains visible, so reconstruction can expose the conflict. -/
def deliver (records : List Segment) (record : Segment) : List Segment :=
  if record ∈ records then records else records ++ [record]

theorem delivery_replay (records : List Segment) (record : Segment) :
    deliver (deliver records record) record = deliver records record := by
  simp only [deliver]
  split <;> simp_all

theorem delivery_retains (records : List Segment) (record old : Segment)
    (h : old ∈ records) : old ∈ deliver records record := by
  simp only [deliver]
  split <;> simp_all

def sourceRecords (records : List Segment) (coordinate : Coordinate) : List Segment :=
  records.filter (fun r => r.coordinate == coordinate)

theorem foreign_source_is_not_selected (records : List Segment) (coordinate : Coordinate)
    (foreign : Segment) (h : foreign.coordinate ≠ coordinate) :
    sourceRecords (records ++ [foreign]) coordinate = sourceRecords records coordinate := by
  simp [sourceRecords, List.filter_append, h]

def closures (records : List Segment) (coordinate : Coordinate) : List Segment :=
  (sourceRecords records coordinate).filter (fun r => r.close.isSome)

/-- Terminal-only records never occupy a data ordinal. Filter BEFORE checking
data twins: a late raw flush beyond a recovered extent is not a conflict. -/
def inExtent (count : Nat) (record : Segment) : Bool :=
  match record.flush with
  | none => false
  | some flush => flush.ordinal < count

def extent (records : List Segment) (coordinate : Coordinate) (count : Nat) :=
  (sourceRecords records coordinate).filter (inExtent count)

theorem terminal_only_not_data (record : Segment) (count : Nat)
    (h : record.flush = none) : inExtent count record = false := by
  simp [inExtent, h]

theorem beyond_extent_inert (records : List Segment) (coordinate : Coordinate)
    (count : Nat) (late : Segment) (h : inExtent count late = false) :
    extent (records ++ [late]) coordinate count = extent records coordinate count := by
  simp [extent, sourceRecords, List.filter_append, h]

theorem late_raw_preserves_closures (records : List Segment)
    (coordinate : Coordinate) (late : Segment) (h : late.close = none) :
    closures (records ++ [late]) coordinate = closures records coordinate := by
  simp [closures, sourceRecords, List.filter_append, h]

end CanonicalOutput
