import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Workflow.Conformance

namespace Conformance.Contracts

open Conformance.ContractCases
open ToolExecution

def workflowBarrierCaseJson
    (witness : Workflow.Conformance.BarrierCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group_terminal_states\":"
      ++ jsonStringArray
        (witness.groupTerminalStates.map ToolCallState.toDefraDB) ++ ","
    ++ "\"synthesis_present\":" ++ boolString witness.synthesisPresent ++ ","
    ++ "\"legal\":" ++ boolString witness.legal
    ++ "}"

def workflowCasesJson : String :=
  jsonArray (Workflow.Conformance.workflowCases.map workflowBarrierCaseJson)

def cancelCauseJson : Option CancelCause → String
  | none => "null"
  | some c => jsonString c.toDefraDB

def toolCallStateJson : ToolCallState → String :=
  fun s => jsonString s.toDefraDB

def toolCallStateOptionJson : Option ToolCallState → String
  | none => "null"
  | some s => toolCallStateJson s

def compositeInterruptCaseJson
    (witness : Workflow.Conformance.CompositeInterruptCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"phase\":" ++ jsonString witness.phase.toContract ++ ","
    ++ "\"parent_state\":" ++ jsonString witness.parentState.toDefraDB ++ ","
    ++ "\"outer_state\":" ++ toolCallStateJson witness.outerState ++ ","
    ++ "\"outer_cancel_cause\":" ++ cancelCauseJson witness.outerCancelCause ++ ","
    ++ "\"fan_out_bridges\":"
      ++ jsonStringArray (witness.fanOutBridges.map ToolCallState.toDefraDB) ++ ","
    ++ "\"synthesis_bridge\":" ++ toolCallStateOptionJson witness.synthesisBridge ++ ","
    ++ "\"continuation_owned\":" ++ boolString witness.continuationOwned ++ ","
    ++ "\"pending_child_cleanup\":" ++ boolString witness.pendingChildCleanup ++ ","
    ++ "\"post_outer_eligible_active\":" ++ boolString witness.postOuterEligibleActive ++ ","
    ++ "\"post_outer_state\":" ++ toolCallStateJson witness.postOuterState ++ ","
    ++ "\"post_outer_cancel_cause\":" ++ cancelCauseJson witness.postOuterCancelCause ++ ","
    ++ "\"post_continuation_owned\":" ++ boolString witness.postContinuationOwned
    ++ "}"

def compositeInterruptCasesJson : String :=
  jsonArray
    (Workflow.Conformance.compositeInterruptCases.map compositeInterruptCaseJson)

end Conformance.Contracts
