import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.ToolExecution
import Proofs.MCPHealth.Executable
import Proofs.BackendHealth.Executable

/-!
# Tool Execution JSON

Serializers for native filesystem, managed-exec, ToolExecution, and MCP health
contract rows.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def nativeFilesystemBoundaryCaseJson
    (witness : NativeFilesystemBoundaryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"tool_name\":" ++ jsonString witness.toolName ++ ","
    ++ "\"work_class\":" ++ jsonString witness.workClass ++ ","
    ++ "\"boundary\":" ++ jsonString witness.boundary ++ ","
    ++ "\"inner_poll_blocks\":" ++ boolString witness.innerPollBlocks ++ ","
    ++ "\"request_deadline_ms\":" ++ toString witness.requestDeadlineMs ++ ","
    ++ "\"blocker_ms\":" ++ toString witness.blockerMs ++ ","
    ++ "\"expected_terminal\":" ++ jsonString witness.expectedTerminal ++ ","
    ++ "\"expected_failure_class\":"
      ++ jsonOptionalString witness.expectedFailureClass ++ ","
    ++ "\"queue_advances_before_blocker_returns\":"
      ++ boolString witness.queueAdvancesBeforeBlockerReturns
    ++ "}"

def managedExecLivenessCaseJson
    (witness : ManagedExecLivenessCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"trigger\":" ++ jsonString witness.trigger ++ ","
    ++ "\"pre_exec_state\":" ++ jsonString witness.preExecState ++ ","
    ++ "\"pre_tool_state\":" ++ jsonString witness.preToolState ++ ","
    ++ "\"expected_exec_state\":"
      ++ jsonString witness.expectedExecState ++ ","
    ++ "\"expected_tool_state\":"
      ++ jsonString witness.expectedToolState ++ ","
    ++ "\"max_steps\":" ++ toString witness.maxSteps ++ ","
    ++ "\"kill_signal_required\":"
      ++ boolString witness.killSignalRequired
    ++ "}"

def toolPreflightCaseJson (witness : ToolExecution.PreflightCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"health\":" ++ jsonString witness.health.toDefraDB ++ ","
    ++ "\"schema_status\":" ++ jsonString witness.schema.toDefraDB ++ ","
    ++ "\"decision\":" ++ jsonString witness.decision.toContract ++ ","
    ++ "\"failure_class\":"
      ++ jsonOptionalString ((witness.decision.failureClass).map ToolExecution.FailureClass.toDefraDB)
    ++ "}"

def toolRetryCaseJson (witness : ToolExecution.RetryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"operation\":" ++ jsonString witness.operation.toDefraDB ++ ","
    ++ "\"idempotency\":" ++ jsonString witness.idempotency.toDefraDB ++ ","
    ++ "\"failure_class\":" ++ jsonString witness.failure.toDefraDB ++ ","
    ++ "\"disposition\":" ++ jsonString witness.disposition.toDefraDB
    ++ "}"

def mcpHealthCaseJson (witness : Proofs.MCPHealth.TransitionCase) : String :=
  let nextCountStr : String :=
    match witness.nextCount with
    | none => "null"
    | some n => toString n
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"start_state\":" ++ jsonString witness.startState.toDefraDB ++ ","
    ++ "\"start_count\":" ++ toString witness.startCount ++ ","
    ++ "\"event\":" ++ jsonString witness.event.toDefraDB ++ ","
    ++ "\"threshold_k\":" ++ toString witness.thresholdK ++ ","
    ++ "\"next_state\":"
      ++ jsonOptionalString
          (witness.nextState.map Proofs.MCPHealth.HealthState.toDefraDB) ++ ","
    ++ "\"next_count\":" ++ nextCountStr ++ ","
    ++ "\"rust_projection\":" ++ jsonOptionalString witness.rustProjection
    ++ "}"

def backendHealthCaseJson (witness : Proofs.BackendHealth.TransitionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"start_state\":" ++ jsonString witness.startState.toDefraDB ++ ","
    ++ "\"start_count\":" ++ toString witness.startCount ++ ","
    ++ "\"event\":" ++ jsonString witness.event.toDefraDB ++ ","
    ++ "\"threshold_k\":" ++ toString witness.thresholdK ++ ","
    ++ "\"next_state\":" ++ jsonString witness.nextState.toDefraDB ++ ","
    ++ "\"next_count\":" ++ toString witness.nextCount ++ ","
    ++ "\"blocks_routing\":" ++ boolString witness.blocksRouting
    ++ "}"

end Conformance.Contracts
