import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases

namespace Conformance.Contracts

open Conformance.ContractCases

def composedInvariantWitnessJson (witness : ComposedInvariantWitness) : String :=
  "{"
    ++ "\"theorem_name\":" ++ jsonString witness.theoremName ++ ","
    ++ "\"witness_kind\":" ++ jsonString witness.witnessKind ++ ","
    ++ "\"scenario\":" ++ jsonString witness.scenario ++ ","
    ++ "\"rust_path\":" ++ jsonString witness.rustPath ++ ","
    ++ "\"trace_step_count\":" ++ toString witness.traceStepCount ++ ","
    ++ "\"transition_path\":" ++ jsonStringArray witness.transitionPath ++ ","
    ++ "\"pre_request_state\":" ++ jsonString witness.preRequestState ++ ","
    ++ "\"pre_request_admission\":" ++ jsonString witness.preRequestAdmission ++ ","
    ++ "\"tool_pre_state\":" ++ jsonString witness.toolPreState ++ ","
    ++ "\"tool_post_state\":" ++ jsonString witness.toolPostState ++ ","
    ++ "\"request_id\":" ++ toString witness.requestId ++ ","
    ++ "\"tool_request_id\":" ++ toString witness.toolRequestId ++ ","
    ++ "\"tool_call_id\":" ++ toString witness.toolCallId ++ ","
    ++ "\"request_deadline\":" ++ toString witness.requestDeadline ++ ","
    ++ "\"request_current_time\":" ++ toString witness.requestCurrentTime ++ ","
    ++ "\"tool_deadline\":" ++ toString witness.toolDeadline ++ ","
    ++ "\"tool_current_time\":" ++ toString witness.toolCurrentTime ++ ","
    ++ "\"deadline_exceeded\":" ++ boolString witness.deadlineExceeded ++ ","
    ++ "\"well_formed_source\":" ++ jsonString witness.wellFormedSource ++ ","
    ++ "\"pre_tool_persisted\":" ++ boolString witness.preToolPersisted ++ ","
    ++ "\"cancel_cause\":" ++ jsonOptionalString witness.cancelCause
    ++ "}"

end Conformance.Contracts
