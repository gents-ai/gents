import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.ReductionEngine

namespace Conformance.Contracts

open Conformance.ContractCases

private def natArray (values : List Nat) : String :=
  jsonArray (values.map toString)

def reductionEngineCaseJson (witness : ReductionEngineCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"source\":" ++ natArray witness.source ++ ","
    ++ "\"input_tokens\":" ++ toString witness.inputTokens ++ ","
    ++ "\"effective_input_budget\":" ++ toString witness.effectiveInputBudget ++ ","
    ++ "\"can_fit\":" ++ boolString witness.canFit ++ ","
    ++ "\"prefix_length\":" ++ toString witness.prefixLength ++ ","
    ++ "\"checkpoint\":" ++ toString witness.checkpoint ++ ","
    ++ "\"threshold_decision\":" ++ jsonString witness.thresholdDecision ++ ","
    ++ "\"decision\":" ++ jsonString witness.decision ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome ++ ","
    ++ "\"not_needed_messages\":" ++ natArray witness.notNeededMessages ++ ","
    ++ "\"compacted_prefix\":" ++ natArray witness.compactedPrefix ++ ","
    ++ "\"retained_suffix\":" ++ natArray witness.retainedSuffix ++ ","
    ++ "\"outcome_checkpoint\":" ++ jsonOptionalNat witness.outcomeCheckpoint ++ ","
    ++ "\"exact\":" ++ boolString witness.exact
    ++ "}"

def reductionEngineCasesJson : String :=
  jsonArray (reductionEngineCases.map reductionEngineCaseJson)

end Conformance.Contracts
