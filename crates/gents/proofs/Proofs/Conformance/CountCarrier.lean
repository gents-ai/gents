import Proofs.Shared.CompletionRetry.CountCarrier
import Proofs.Conformance.Family

namespace Conformance.CountCarrier
open Lean CompletionRetry.CountCarrier

structure FieldCase where
  schema : String
  field : Field

private def scalars : List FieldCase :=
  [⟨"Int", .integer⟩, ⟨"Float", .number⟩, ⟨"Float32", .number⟩,
   ⟨"Float64", .number⟩, ⟨"String", .text⟩, ⟨"ID", .docId⟩,
   ⟨"Blob", .text⟩, ⟨"JSON", .json⟩, ⟨"DateTime", .dateTime⟩,
   ⟨"Boolean", .boolean⟩]

def fieldCases : List FieldCase :=
  scalars.flatMap (fun c => [c, ⟨c.schema ++ "!", c.field⟩]) ++
  [⟨"[String]", .list⟩, ⟨"[String!]", .list⟩, ⟨"[Int]", .list⟩,
   ⟨"LIST", .list⟩, ⟨"NON_NULL", .unsupported⟩,
   ⟨"Object", .relation⟩, ⟨"[Object]", .list⟩, ⟨"Unknown", .unsupported⟩]

def fieldOf (schema : String) : Field :=
  ((fieldCases.find? (·.schema == schema)).map (·.field)).getD .unsupported

/-- A wire value as its kind and its JSON spelling: a `number` or `other`
spelling is JSON text, a `text` spelling is the string itself. The family JSON
type cannot carry `1e1` apart from `10`. -/
structure WireInput where
  kind : String
  spelling : String
  deriving ToJson

def WireInput.ofModel : WireValue → WireInput
  | .number raw => ⟨"number", raw⟩
  | .text text => ⟨"text", text⟩
  | .other raw => ⟨"other", raw⟩

def WireInput.toModel (wire : WireInput) : WireValue :=
  match wire.kind with
  | "number" => .number wire.spelling
  | "text" => .text wire.spelling
  | _ => .other wire.spelling

structure ValueInput where
  value : WireInput
  maximum : Nat
  deriving ToJson

structure ValueCase where
  value : WireValue
  maximum : Nat := 1000

def valueCases : List ValueCase :=
  [⟨.number "10", 1000⟩, ⟨.text "10", 1000⟩,
   ⟨.number "0", 1000⟩, ⟨.text "0", 1000⟩,
   ⟨.number "256", 256⟩, ⟨.number "257", 256⟩,
   ⟨.number "1000", 1000⟩, ⟨.number "1001", 1000⟩,
   ⟨.text "1001", 1000⟩, ⟨.number "10", 0⟩,
   ⟨.text "18446744073709551616", 1000⟩] ++
  (["01", "+1", "-1", "1.0", " 1", "1 ", "１", ""].map
    (fun text => ⟨.text text, 1000⟩)) ++
  (["1.0", "1e1", "-1"].map (fun raw => ⟨.number raw, 1000⟩)) ++
  (["null", "true", "[]", "{}"].map (fun raw => ⟨.other raw, 1000⟩))

@[conformance_family]
def value : Family ValueInput (Option Nat) where
  inputs := valueCases.map fun c => ⟨WireInput.ofModel c.value, c.maximum⟩
  model := fun i => parseWire i.value.toModel i.maximum

structure FieldInput where
  schema : String
  deriving ToJson

@[conformance_family]
def admission : Family FieldInput Bool where
  inputs := fieldCases.map fun c => ⟨c.schema⟩
  model := fun i => canCarry (fieldOf i.schema)

private def witnessValue (field : Field) : WireValue :=
  wireWitness (if admitsEncoding field .integer then .integer else .decimal)

private def probeValue (field : Field) : WireValue :=
  if field == .dateTime then .text "2026-10-09T00:00:00Z" else witnessValue field

/-- The event trigger group bound production reads counts against. -/
def groupMaximum : Nat := 256

structure StorageInput where
  schema : String
  probe : WireInput
  deriving ToJson

structure StorageOutput where
  argument : Option Nat
  stored : Option Nat
  introspected : Bool
  decimalAdmitted : Bool
  deriving ToJson

/-- A bounded write tool stores one scalar count field. Lists, relations and
unsupported shapes declare no scalar field to write, and DefraDB refuses a
non-null `ID` declaration, so only scalar carriers and `DateTime` are written.
Every field is also offered the decimal witness `"10"`, whether or not it
carries a count: a field takes it only when its input kinds include text
(`admitsEncoding field .decimal`). -/
@[conformance_family]
def storage : Family StorageInput StorageOutput where
  inputs := (fieldCases.filter fun c => canCarry c.field || c.field == .dateTime).map
    fun c => ⟨c.schema, WireInput.ofModel (probeValue c.field)⟩
  model := fun i =>
    let field := fieldOf i.schema
    let parsed := parseWire i.probe.toModel groupMaximum
    { argument := parsed
      stored := parsed
      introspected := canCarry field
      decimalAdmitted := admitsEncoding field .decimal }

structure IntegerTextInput where
  schema : String
  text : String
  deriving ToJson

/-- Count spellings sent as JSON text to an `Int` field of a bounded write
tool: the canonical decimal, the non-canonical forms `parseWire` refuses, a
count past the group bound and zero. Maintainer question (2026-11-13): should
an `Int` field take a count sent as a decimal string? The production writer
(`defra_write::input::literal`) takes only a JSON integer there and refuses
every text, canonical or not, with "values are not coerced", so the model
keeps `integer_field_takes_no_text` and this family holds production to it. -/
def integerTexts : List String :=
  ["10", "+10", "010", " 10", "10 ", "1e1", "10.0", "１０", "-1", "0", "257", ""]

@[conformance_family]
def integerText : Family IntegerTextInput Bool where
  inputs := ["Int", "Int!"].flatMap fun schema => integerTexts.map fun text => ⟨schema, text⟩
  model := fun i => admitsEncoding (fieldOf i.schema) .decimal

/-- The obligation fixture collection declares one field of each reported type
that can never carry a count; a count-carrying field passes on to validation
this scenario does not configure. -/
@[conformance_family]
def obligationField : Family FieldInput Bool where
  inputs := ["Boolean", "LIST", "DateTime", "DateTime!", "ID"].map fun schema => ⟨schema⟩
  model := fun i => !canCarry (fieldOf i.schema)

/-- The event source fixture collection declares one field of each reported
type that can never carry a count; a count-carrying field passes on to
validation this scenario does not configure. -/
@[conformance_family]
def eventSourceField : Family FieldInput Bool where
  inputs := ["Boolean", "LIST", "DateTime", "DateTime!", "ID"].map fun schema => ⟨schema⟩
  model := fun i => !canCarry (fieldOf i.schema)

example : fieldCases.all (fun c => fieldOf c.schema == c.field) = true := by native_decide

end Conformance.CountCarrier
