import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.SelfConfig.Cases

/-!
# Self-Configuration JSON

Serializers for the SelfConfig field tables (per-target writable/protected
partitions) and patch-merge witness cases.
-/

namespace Conformance.Contracts

open SelfConfig SelfConfig.ContractCases

private def scBool (b : Bool) : String :=
  if b then "true" else "false"

def selfConfigFieldTableJson (t : Target) : String :=
  "{"
    ++ "\"collection\":" ++ jsonString t.collectionName ++ ","
    ++ "\"unique_field\":" ++ jsonString t.uniqueField ++ ","
    ++ "\"category\":" ++ jsonString t.category ++ ","
    ++ "\"all_fields\":" ++ jsonStringArray (allFields t) ++ ","
    ++ "\"writable_fields\":" ++ jsonStringArray (writableFields t) ++ ","
    ++ "\"protected_fields\":" ++ jsonStringArray (protectedFields t)
  ++ "}"

def selfConfigFieldTablesJson : String :=
  jsonArray (allTargets.map selfConfigFieldTableJson)

def fieldValuePairJson (entry : FieldKey × FieldValue) : String :=
  "{"
    ++ "\"field\":" ++ jsonString entry.1 ++ ","
    ++ "\"value\":" ++ jsonString entry.2
  ++ "}"

def selfConfigPatchEntryJson (entry : FieldKey × Option FieldValue) : String :=
  "{"
    ++ "\"field\":" ++ jsonString entry.1 ++ ","
    ++ "\"action\":"
      ++ (match entry.2 with
          | some _ => jsonString "set"
          | none => jsonString "clear") ++ ","
    ++ "\"value\":"
      ++ (match entry.2 with
          | some v => jsonString v
          | none => "null")
  ++ "}"

def selfConfigCaseJson (w : CaseWitness) : String :=
  "{"
    ++ "\"name\":" ++ jsonString w.row.name ++ ","
    ++ "\"collection\":" ++ jsonString w.row.target.collectionName ++ ","
    ++ "\"category\":" ++ jsonString w.row.target.category ++ ","
    ++ "\"guarded\":" ++ scBool w.row.guarded ++ ","
    ++ "\"validates\":" ++ scBool w.row.validates ++ ","
    ++ "\"doc\":" ++ jsonArray (w.row.doc.map fieldValuePairJson) ++ ","
    ++ "\"patch\":"
      ++ jsonArray (w.row.patch.map selfConfigPatchEntryJson) ++ ","
    ++ "\"admissible\":" ++ scBool w.admissiblePatch ++ ","
    ++ "\"accepted\":" ++ scBool w.accepted ++ ","
    ++ "\"result\":" ++ jsonArray (w.result.map fieldValuePairJson) ++ ","
    ++ "\"protected_preserved\":" ++ scBool w.protectedPreserved ++ ","
    ++ "\"containment_holds\":" ++ scBool w.containmentHolds ++ ","
    ++ "\"unchanged_on_reject\":" ++ scBool w.unchangedOnReject ++ ","
    ++ "\"gate_on_after_accept\":" ++ scBool w.gateOnAfterAccept
  ++ "}"

def selfConfigCasesJson : String :=
  jsonArray (selfConfigCases.map selfConfigCaseJson)

end Conformance.Contracts
