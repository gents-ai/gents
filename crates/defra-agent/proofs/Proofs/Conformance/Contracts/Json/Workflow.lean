import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Workflow.Conformance

/-!
# Workflow JSON contracts
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def workflowBarrierCaseJson
    (witness : Workflow.Conformance.BarrierCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group_terminal_states\":"
      ++ jsonStringArray
        (witness.groupTerminalStates.map ToolExecution.ToolCallState.toDefraDB) ++ ","
    ++ "\"synthesis_present\":" ++ boolString witness.synthesisPresent ++ ","
    ++ "\"legal\":" ++ boolString witness.legal
    ++ "}"

def workflowCasesJson : String :=
  jsonArray (Workflow.Conformance.workflowCases.map workflowBarrierCaseJson)

end Conformance.Contracts
