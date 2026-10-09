import Proofs.CompletionRetry.CountCarrier
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.CountCarrier
open CompletionRetry.CountCarrier Conformance.Contracts

structure FieldCase where
  schema : String
  field : Field

private def scalars : List FieldCase :=
  [⟨"Int", .integer⟩, ⟨"Float", .number⟩, ⟨"Float32", .number⟩,
   ⟨"Float64", .number⟩, ⟨"String", .text⟩, ⟨"ID", .text⟩,
   ⟨"Blob", .text⟩, ⟨"JSON", .json⟩, ⟨"DateTime", .dateTime⟩,
   ⟨"Boolean", .boolean⟩]

def fieldCases : List FieldCase :=
  scalars.flatMap (fun c => [c, ⟨c.schema ++ "!", c.field⟩]) ++
  [⟨"[String]", .list⟩, ⟨"[String!]", .list⟩, ⟨"[Int]", .list⟩,
   ⟨"LIST", .list⟩, ⟨"NON_NULL", .unsupported⟩,
   ⟨"Object", .relation⟩, ⟨"[Object]", .list⟩, ⟨"Unknown", .unsupported⟩]

private def wireJson : WireValue → String
  | .number raw | .other raw => raw
  | .text text => jsonString text


private def witnessValue (field : Field) : WireValue :=
  wireWitness (if admitsEncoding field .integer then .integer else .decimal)

private def probeValue (field : Field) : WireValue :=
  if field == .dateTime then .text "2026-10-09T00:00:00Z" else witnessValue field

/-- DefraDB has no non-nillable DocID scalar. ID! remains a spelling-adapter
case, but cannot appear on a real introspected field; native binding must
assert SDL rejection rather than claim a storage witness for it. -/
private def schemaError (c : FieldCase) : Option String :=
  if c.schema == "ID!" then some "NonNull variant for type ID is not supported" else none

private def fieldCaseJson (c : FieldCase) : String :=
  "{\"schema\":" ++ jsonString c.schema ++
  ",\"accepted\":" ++ (if canCarry c.field then "true" else "false") ++
  ",\"witness\":" ++ (if canCarry c.field then wireJson (witnessValue c.field) else "null") ++
  ",\"storage\":" ++ (if (schemaError c).isNone && (canCarry c.field || c.field == .dateTime) then "true" else "false") ++
  ",\"schema_error\":" ++ ((schemaError c).map jsonString).getD "null" ++
  ",\"probe\":" ++ wireJson (probeValue c.field) ++
  ",\"probe_expected\":" ++ ((parseWire (probeValue c.field) 256).map toString).getD "null" ++ "}"

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


private def valueCaseJson (c : ValueCase) : String :=
  "{\"value\":" ++ wireJson c.value ++ ",\"maximum\":" ++ toString c.maximum ++
  ",\"expected\":" ++ ((parseWire c.value c.maximum).map toString).getD "null" ++ "}"

def casesJson : String :=
  "{\"fields\":" ++ jsonArray (fieldCases.map fieldCaseJson) ++
  ",\"values\":" ++ jsonArray (valueCases.map valueCaseJson) ++ "}"
end Conformance.CountCarrier
