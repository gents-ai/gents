import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.DescendantGraph

namespace Conformance.Contracts

open Conformance.ContractCases

def descendantGraphCaseJson (value : DescendantGraphCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString value.name ++ ","
    ++ "\"root_request_id\":" ++ toString value.rootRequestId ++ ","
    ++ "\"parent_request_id\":" ++ toString value.parentRequestId ++ ","
    ++ "\"child_request_id\":" ++ toString value.childRequestId ++ ","
    ++ "\"await_mode\":" ++ jsonString value.awaitMode ++ ","
    ++ "\"materialization\":" ++ jsonString value.materialization ++ ","
    ++ "\"lifecycle\":" ++ jsonString value.lifecycle ++ ","
    ++ "\"direct\":" ++ boolString value.direct ++ ","
    ++ "\"visible\":" ++ boolString value.visible ++ ","
    ++ "\"readable\":" ++ boolString value.readable ++ ","
    ++ "\"retryable\":" ++ boolString value.retryable ++ ","
    ++ "\"listed_by_default\":" ++ boolString value.listedByDefault ++ ","
    ++ "\"controllable\":" ++ boolString value.controllable ++ ","
    ++ "\"cursor_anchor_survives_terminal\":"
      ++ boolString value.cursorAnchorSurvivesTerminal
    ++ ",\"caller_session\":" ++ jsonString value.callerSession
    ++ ",\"caller_agent\":" ++ jsonString value.callerAgent
    ++ ",\"caller_requester\":" ++ (match value.callerRequester with
        | none => "null"
        | some requester => jsonString requester)
    ++ ",\"session_authorized\":" ++ boolString value.sessionAuthorized
    ++ ",\"session_controllable\":" ++ boolString value.sessionControllable
    ++ ",\"child_state\":" ++ (match value.childState with
        | none => "null"
        | some state => jsonString state)
    ++ ",\"spawn_unclaimed\":" ++ boolString value.spawnUnclaimed
    ++ ",\"cancel_intent\":" ++ boolString value.cancelIntent
    ++ ",\"steer_admission\":" ++ jsonString value.steerAdmission
    ++ "}"

def descendantGraphCasesJson : String :=
  jsonArray (descendantGraphCases.map descendantGraphCaseJson)

def descendantCursorEdgeJson (value : DescendantCursorEdge) : String :=
  "{"
    ++ "\"tool_call_id\":" ++ toString value.toolCallId ++ ","
    ++ "\"child_request_id\":" ++ toString value.childRequestId ++ ","
    ++ "\"lifecycle\":" ++ jsonString value.lifecycle
    ++ "}"

def descendantCursorCaseJson (value : DescendantCursorCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString value.name ++ ","
    ++ "\"edges\":" ++ jsonArray (value.edges.map descendantCursorEdgeJson) ++ ","
    ++ "\"after\":" ++ (match value.after with
        | none => "null"
        | some (tool, child) =>
            "{\"tool_call_id\":" ++ toString tool
              ++ ",\"child_request_id\":" ++ toString child ++ "}") ++ ","
    ++ "\"anchor_settled\":" ++ boolString value.anchorSettled ++ ","
    ++ "\"expected_child_request_ids\":"
      ++ jsonArray (value.expectedChildRequestIds.map toString) ++ ","
    ++ "\"stale_cursor\":" ++ boolString value.staleCursor
    ++ "}"

def descendantCursorCasesJson : String :=
  jsonArray (descendantCursorCases.map descendantCursorCaseJson)

end Conformance.Contracts
