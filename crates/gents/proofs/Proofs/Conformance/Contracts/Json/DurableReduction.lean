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
    ++ "\"claim_commit\":" ++ toString witness.claimCommit ++ ","
    ++ "\"prior_checkpoint\":" ++ jsonOptionalNat witness.priorCheckpoint ++ ","
    ++ "\"prior_claim_commit\":" ++ jsonOptionalNat witness.priorClaimCommit ++ ","
    ++ "\"pair_closed\":" ++ boolString witness.pairClosed ++ ","
    ++ "\"inference_cites\":" ++ boolString witness.inferenceCites ++ ","
    ++ "\"inference_supported\":" ++ boolString witness.inferenceSupported ++ ","
    ++ "\"title_cites\":" ++ boolString witness.titleCites ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome ++ ","
    ++ "\"durable_after\":" ++ boolString witness.durableAfter ++ ","
    ++ "\"send_permitted\":" ++ boolString witness.sendPermitted ++ ","
    ++ "\"consumed\":" ++ boolString witness.consumed
    ++ "}"

def durableReductionCasesJson : String :=
  jsonArray (durableReductionCases.map durableReductionCaseJson)

end Conformance.Contracts
