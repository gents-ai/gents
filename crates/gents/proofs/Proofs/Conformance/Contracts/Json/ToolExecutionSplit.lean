import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.ToolExecutionSplit

namespace Conformance.Contracts

open Conformance.ContractCases

def toolExecutionSplitCaseJson (row : ToolExecutionSplitCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString row.name ++ ","
    ++ "\"operation\":" ++ jsonString row.operation ++ ","
    ++ "\"disposition\":" ++ jsonString row.disposition ++ ","
    ++ "\"exact_projection\":" ++ boolString row.exactProjection ++ ","
    ++ "\"output_pins_running\":" ++ boolString row.outputPinsRunning ++ ","
    ++ "\"terminal_output_closed\":" ++ boolString row.terminalOutputClosed ++ ","
    ++ "\"owner_preserved\":" ++ boolString row.ownerPreserved ++ ","
    ++ "\"approval_pins_held\":" ++ boolString row.approvalPinsHeld ++ ","
    ++ "\"immutable_noop\":" ++ boolString row.immutableNoop
    ++ "}"

def toolExecutionSplitCasesJson : String :=
  jsonArray (toolExecutionSplitCases.map toolExecutionSplitCaseJson)

end Conformance.Contracts
