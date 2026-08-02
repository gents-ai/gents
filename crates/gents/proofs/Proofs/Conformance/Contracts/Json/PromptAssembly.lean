import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.PromptAssembly

namespace Conformance.Contracts

open Conformance.ContractCases

def jsonNatArray (values : List Nat) : String :=
  jsonArray (values.map toString)

def promptAssemblyItemJson (item : PromptAssemblyItemCase) : String :=
  "{"
    ++ "\"item\":" ++ jsonString item.item ++ ","
    ++ "\"value\":" ++ toString item.value
    ++ "}"

def promptAssemblyRowJson (row : PromptAssemblyRowCase) : String :=
  "{"
    ++ "\"role\":" ++ jsonString row.role ++ ","
    ++ "\"kind\":" ++ jsonString row.kind ++ ","
    ++ "\"call_ids\":" ++ jsonNatArray row.callIds ++ ","
    ++ "\"content\":" ++ jsonArray (row.content.map promptAssemblyItemJson)
    ++ "}"

def promptAssemblyRowsJson (rows : List PromptAssemblyRowCase) : String :=
  jsonArray (rows.map promptAssemblyRowJson)

def promptAssemblySplitJson (split : PromptAssemblySplitCase) : String :=
  "{"
    ++ "\"index\":" ++ toString split.index ++ ","
    ++ "\"expected\":" ++ promptAssemblyRowsJson split.expected
    ++ "}"

def promptAssemblySanitizeCaseJson (witness : PromptAssemblySanitizeCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"input\":" ++ promptAssemblyRowsJson witness.input ++ ","
    ++ "\"expected\":" ++ promptAssemblyRowsJson witness.expected ++ ","
    ++ "\"expected_twice\":" ++ promptAssemblyRowsJson witness.expectedTwice ++ ","
    ++ "\"splits\":" ++ jsonArray (witness.splits.map promptAssemblySplitJson)
    ++ "}"

def promptAssemblySanitizeCasesJson : String :=
  jsonArray (promptAssemblySanitizeCases.map promptAssemblySanitizeCaseJson)

def promptAssemblyLayerCaseJson (witness : PromptAssemblyLayerCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"skill_count\":" ++ toString witness.skillCount ++ ","
    ++ "\"summary_count\":" ++ toString witness.summaryCount ++ ","
    ++ "\"conversation_len\":" ++ toString witness.conversationLen ++ ","
    ++ "\"slots\":" ++ jsonStringArray witness.slots
    ++ "}"

def promptAssemblyLayerCasesJson : String :=
  jsonArray (promptAssemblyLayerCases.map promptAssemblyLayerCaseJson)

def promptAssemblyRepairCaseJson (witness : PromptAssemblyRepairCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"input\":" ++ jsonString witness.input ++ ","
    ++ "\"expected\":" ++ jsonString witness.expected ++ ","
    ++ "\"expected_twice\":" ++ jsonString witness.expectedTwice ++ ","
    ++ "\"payload_only\":" ++ boolString witness.payloadOnly
    ++ "}"

def promptAssemblyRepairCasesJson : String :=
  jsonArray (promptAssemblyRepairCases.map promptAssemblyRepairCaseJson)

end Conformance.Contracts
