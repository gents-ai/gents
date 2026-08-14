import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.DurableReduction

namespace Conformance.Contracts

open Conformance.ContractCases

def durableReductionCaseJson (witness : DurableReductionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"request_doc_id\":" ++ toString witness.requestDocId ++ ","
    ++ "\"turn_index\":" ++ toString witness.turnIndex ++ ","
    ++ "\"ordinal\":" ++ toString witness.ordinal ++ ","
    ++ "\"checkpoint\":" ++ toString witness.checkpoint ++ ","
    ++ "\"prior_checkpoint\":" ++ jsonOptionalNat witness.priorCheckpoint ++ ","
    ++ "\"pair_closed\":" ++ boolString witness.pairClosed ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome ++ ","
    ++ "\"durable_after\":" ++ boolString witness.durableAfter ++ ","
    ++ "\"send_permitted\":" ++ boolString witness.sendPermitted
    ++ "}"

def durableReductionCasesJson : String :=
  jsonArray (durableReductionCases.map durableReductionCaseJson)

end Conformance.Contracts
