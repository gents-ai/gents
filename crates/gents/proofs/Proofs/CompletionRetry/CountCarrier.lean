import Proofs.ToolPolicy.WriteInput
import Mathlib.Data.Nat.Basic

namespace CompletionRetry.CountCarrier
open ToolPolicy.WriteInput

inductive Field where
  | integer | number | text | json | dateTime | boolean | list | relation | unsupported
  deriving DecidableEq, Repr

inductive Encoding where
  | integer | decimal
  deriving DecidableEq, Repr

def encodingKind : Encoding → Kind
  | .integer => .integer
  | .decimal => .text

def inputKinds : Field → List Kind
  | .integer => [.integer]
  | .number => [.number]
  | .text | .dateTime => [.text]
  | .json => [.object, .array, .text, .number, .boolean]
  | .boolean => [.boolean]
  | .list => [.array]
  | .relation | .unsupported => []

/-- Native DateTime write arguments are text; DefraDB parses that text as
RFC3339 and renders stored DateTime values as RFC3339. A canonical decimal
string is not RFC3339. Other admitted count encodings have a storage witness
(including whole-valued floating fields, returned as JSON integers). These
external scalar constraints are bound by real storage conformance cases. -/
def storageAdmits (field : Field) (_encoding : Encoding) : Bool :=
  field != .dateTime

def admitsEncoding (field : Field) (encoding : Encoding) : Bool :=
  (inputKinds field).any (fun expected => compatible expected (encodingKind encoding)) &&
    storageAdmits field encoding

def canCarry (field : Field) : Bool :=
  admitsEncoding field .integer || admitsEncoding field .decimal

/-- Encodings classify JSON integers and canonical decimal strings separately.
A noncanonical string, fractional number, null, bool, array or object has no
count encoding. Bounds are checked before either provenance consumes it. -/
def parseCount (encoding : Option Encoding) (value maximum : Nat) : Option Nat :=
  if encoding.isSome && 0 < value && value ≤ maximum then some value else none

inductive WireValue where
  | number (spelling : String)
  | text (value : String)
  | other (json : String)
  deriving Repr

/-- The decimal spelling is ASCII, unsigned, and has no redundant leading
zero. Numeric JSON fractions and exponent spellings stay outside the integer
channel, matching serde_json's integer representation rather than coercing. -/
def canonicalNat (text : String) : Option Nat :=
  if text.isEmpty || !(text.toList.all (fun c => '0' ≤ c && c ≤ '9')) ||
      (text.startsWith "0" && text != "0") then none
  else text.toNat?

def classify : WireValue → Option (Encoding × Nat)
  | .number spelling => (canonicalNat spelling).map (fun n => (.integer, n))
  | .text text => (canonicalNat text).map (fun n => (.decimal, n))
  | .other _ => none

def parseWire (wire : WireValue) (maximum : Nat) : Option Nat := do
  let (encoding, n) ← classify wire
  parseCount (some encoding) n maximum

theorem noncanonical_decimal_rejected :
    parseWire (.text "01") 1000 = none ∧
    parseWire (.text "+1") 1000 = none ∧
    parseWire (.text "１") 1000 = none ∧
    parseWire (.number "1.0") 1000 = none ∧
    parseWire (.number "1e1") 1000 = none := by native_decide

def carries (field : Field) (encoding : Encoding) (value maximum : Nat) : Prop :=
  admitsEncoding field encoding = true ∧ parseCount (some encoding) value maximum = some value

/-- Admission means some successful native input can carry a count, not that
all values of the field type are counts or that every large integer survives
floating-point storage. Ten is representable by every admitted scalar,
including hex-encoded Blob text. -/
theorem admission_iff_witness (field : Field) (maximum : Nat) (h : 10 ≤ maximum) :
    canCarry field = true ↔ ∃ encoding value, carries field encoding value maximum := by
  constructor
  · intro accepted
    simp only [canCarry, Bool.or_eq_true] at accepted
    rcases accepted with accepted | accepted
    · exact ⟨.integer, 10, accepted, by simp [parseCount]; omega⟩
    · exact ⟨.decimal, 10, accepted, by simp [parseCount]; omega⟩
  · rintro ⟨encoding, value, accepted, _⟩
    cases encoding <;> simp_all [canCarry]

def wireWitness : Encoding → WireValue
  | .integer => .number "10"
  | .decimal => .text "10"

theorem wire_witness_parses (encoding : Encoding) (maximum : Nat) (h : 10 ≤ maximum) :
    parseWire (wireWitness encoding) maximum = some 10 := by
  have decimal : canonicalNat "10" = some 10 := by native_decide
  cases encoding <;> simp [wireWitness, parseWire, classify, decimal, parseCount, h]

theorem admission_iff_wire_witness (field : Field) (maximum : Nat) (h : 10 ≤ maximum) :
    canCarry field = true ↔ ∃ encoding,
      admitsEncoding field encoding = true ∧
      parseWire (wireWitness encoding) maximum = some 10 := by
  constructor
  · intro accepted
    simp only [canCarry, Bool.or_eq_true] at accepted
    rcases accepted with accepted | accepted
    · exact ⟨.integer, accepted, wire_witness_parses .integer maximum h⟩
    · exact ⟨.decimal, accepted, wire_witness_parses .decimal maximum h⟩
  · rintro ⟨encoding, accepted, _⟩
    cases encoding <;> simp_all [canCarry]

theorem datetime_has_no_count : canCarry .dateTime = false := by decide

theorem noncanonical_has_no_count (value maximum : Nat) :
    parseCount none value maximum = none := by simp [parseCount]

theorem bounded_positive (encoding : Option Encoding) (value maximum result : Nat)
    (h : parseCount encoding value maximum = some result) :
    result = value ∧ 0 < result ∧ result ≤ maximum := by
  simp only [parseCount] at h
  split at h <;> simp_all

end CompletionRetry.CountCarrier
