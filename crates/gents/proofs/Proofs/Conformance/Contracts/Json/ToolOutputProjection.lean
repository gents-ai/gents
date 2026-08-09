import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.ToolOutputProjection

namespace Conformance.Contracts

open Conformance.ContractCases

def toolOutputProjectionCaseJson (row : ToolOutputProjectionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString row.name ++ ","
    ++ "\"observation\":" ++ jsonString row.observation ++ ","
    ++ "\"observed_hash\":" ++ toString row.observedHash ++ ","
    ++ "\"accepted\":" ++ boolString row.accepted ++ ","
    ++ "\"full_output_preserved\":" ++ boolString row.fullOutputPreserved
    ++ "}"

def toolOutputProjectionCasesJson : String :=
  jsonArray (toolOutputProjectionCases.map toolOutputProjectionCaseJson)

end Conformance.Contracts
