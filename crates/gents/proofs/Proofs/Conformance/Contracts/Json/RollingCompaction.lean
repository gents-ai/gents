import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.RollingCompaction

namespace Conformance.Contracts

open Conformance.ContractCases

private def natArray (values : List Nat) : String :=
  jsonArray (values.map toString)

private def boolArray (values : List Bool) : String :=
  jsonArray (values.map boolString)

def rollingCompactionCaseJson (witness : RollingCompactionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"before_cursor\":" ++ toString witness.beforeCursor ++ ","
    ++ "\"target_messages\":" ++ toString witness.targetMessages ++ ","
    ++ "\"chunk_messages\":" ++ natArray witness.chunkMessages ++ ","
    ++ "\"chunk_pair_closed\":" ++ boolArray witness.chunkPairClosed ++ ","
    ++ "\"chunk_can_dispatch\":" ++ boolArray witness.chunkCanDispatch ++ ","
    ++ "\"checkpoint_covered\":" ++ toString witness.checkpointCovered ++ ","
    ++ "\"completed\":" ++ boolString witness.completed ++ ","
    ++ "\"plan_valid\":" ++ boolString witness.planValid ++ ","
    ++ "\"cursor_after\":" ++ toString witness.cursorAfter ++ ","
    ++ "\"prior_payload\":" ++ jsonOptionalNat witness.priorPayload ++ ","
    ++ "\"next_chunk\":" ++ natArray witness.nextChunk ++ ","
    ++ "\"step_input\":" ++ natArray witness.stepInput
    ++ "}"

def rollingCompactionCasesJson : String :=
  jsonArray (rollingCompactionCases.map rollingCompactionCaseJson)

end Conformance.Contracts
