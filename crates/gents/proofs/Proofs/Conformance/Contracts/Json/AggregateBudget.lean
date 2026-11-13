import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.AggregateBudget

namespace Conformance.Contracts

open Conformance.ContractCases

private def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

private def rehydrationCaseJson (c : BudgetRehydrationCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"request_doc_id\":" ++ jsonString c.requestDocId ++
  ",\"pinned_limit\":" ++ optionalNatJson c.pinnedLimit ++
  ",\"rows\":" ++ jsonArray (c.rows.map (fun row =>
    "{\"request_doc_id\":" ++ jsonString row.requestDocId ++
    ",\"kind\":" ++ jsonString (match row.kind with
      | .inference => "inference" | .compaction => "compaction") ++
    ",\"prompt_tokens\":" ++ toString row.usage.promptTokens ++
    ",\"completion_tokens\":" ++ toString row.usage.completionTokens ++ "}")) ++
  ",\"ledger\":" ++ (c.result.map (fun ledger =>
    "{\"limit\":" ++ toString ledger.limit ++
    ",\"used\":" ++ toString ledger.used ++ "}") |>.getD "null") ++ "}"

def budgetRehydrationCasesJson : String :=
  jsonArray (budgetRehydrationCases.map rehydrationCaseJson)

end Conformance.Contracts
