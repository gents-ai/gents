import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.AggregateBudget

namespace Conformance.Contracts

open Conformance.ContractCases

private def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def aggregateTokenBudgetCaseJson (witness : AggregateTokenBudgetCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"limit\":" ++ toString witness.limit ++ ","
    ++ "\"used\":" ++ toString witness.used ++ ","
    ++ "\"request_doc_id\":" ++ jsonString witness.requestDocId ++ ","
    ++ "\"prior_request_doc_ids\":" ++ jsonStringArray witness.priorRequestDocIds ++ ","
    ++ "\"prior_call_kinds\":" ++ jsonStringArray witness.priorCallKinds ++ ","
    ++ "\"prior_prompt_tokens\":"
      ++ jsonArray (witness.priorPromptTokens.map toString) ++ ","
    ++ "\"prior_completion_tokens\":"
      ++ jsonArray (witness.priorCompletionTokens.map toString) ++ ","
    ++ "\"input_tokens\":" ++ toString witness.inputTokens ++ ","
    ++ "\"configured_max_output_tokens\":"
      ++ toString witness.configuredMaxOutputTokens ++ ","
    ++ "\"reported_input_tokens\":" ++ toString witness.reportedInputTokens ++ ","
    ++ "\"reported_output_tokens\":" ++ toString witness.reportedOutputTokens ++ ","
    ++ "\"reported_total_tokens\":" ++ toString witness.reportedTotalTokens ++ ","
    ++ "\"usage_present\":" ++ boolString witness.usagePresent ++ ","
    ++ "\"terminal_valid\":" ++ boolString witness.terminalValid ++ ","
    ++ "\"effective_output_tokens\":" ++ toString witness.effectiveOutputTokens ++ ","
    ++ "\"can_dispatch\":" ++ boolString witness.canDispatch ++ ","
    ++ "\"charged_tokens\":" ++ toString witness.chargedTokens ++ ","
    ++ "\"charge_result\":" ++ jsonString witness.chargeResult ++ ","
    ++ "\"next_used\":" ++ optionalNatJson witness.nextUsed
    ++ ",\"post_charge_action\":" ++ jsonString witness.postChargeAction
    ++ "}"

def aggregateTokenBudgetCasesJson : String :=
  jsonArray (aggregateTokenBudgetCases.map aggregateTokenBudgetCaseJson)

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
