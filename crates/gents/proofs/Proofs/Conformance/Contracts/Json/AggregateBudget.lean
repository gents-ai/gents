import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.AggregateBudget
import Proofs.Conformance.Family

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
    ++ ",\"abandoned_used\":" ++ optionalNatJson witness.abandonedUsed
    ++ ",\"post_charge_action\":" ++ jsonString witness.postChargeAction
    ++ "}"

def aggregateTokenBudgetCasesJson : String :=
  jsonArray (aggregateTokenBudgetCases.map aggregateTokenBudgetCaseJson)

end Conformance.Contracts

namespace Conformance.BudgetRehydration

open Lean PromptAssembly.AggregateBudget Conformance.ContractCases

deriving instance ToJson for CallKind, Ledger

structure RowWire where
  requestDocId : String
  kind : CallKind
  promptTokens : Nat
  completionTokens : Nat
  deriving ToJson

structure RehydrationInput where
  name : String
  requestDocId : String
  pinnedLimit : Option Nat
  rows : List RowWire
  deriving ToJson

@[conformance_family]
def ledger : Family RehydrationInput (Option Ledger) where
  inputs := budgetRehydrationCases.map fun c =>
    { name := c.name, requestDocId := c.requestDocId, pinnedLimit := c.pinnedLimit
      rows := c.rows.map fun row =>
        { requestDocId := row.requestDocId, kind := row.kind
          promptTokens := row.usage.promptTokens
          completionTokens := row.usage.completionTokens } }
  model := fun i =>
    rehydrateRequest i.requestDocId i.pinnedLimit
      (i.rows.map fun row => ⟨row.requestDocId, row.kind, ⟨row.promptTokens, row.completionTokens⟩⟩)

end Conformance.BudgetRehydration
