import Proofs.Conformance.ClientShell.Contracts.Cases

/-!
# ClientShell Contract JSON
-/

namespace Conformance.ClientShellContracts

open Conformance.Contracts

def ClientShellContractCase.toJson (witness : ClientShellContractCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"property\":" ++ jsonString witness.property ++ ","
    ++ "\"input\":" ++ jsonString witness.input ++ ","
    ++ "\"pre_selection_agent\":" ++ jsonNatOption witness.preSelectionAgent ++ ","
    ++ "\"pre_selection_session\":" ++ jsonNatOption witness.preSelectionSession ++ ","
    ++ "\"post_selection_agent\":" ++ jsonNatOption witness.postSelectionAgent ++ ","
    ++ "\"post_selection_session\":" ++ jsonNatOption witness.postSelectionSession ++ ","
    ++ "\"pre_workflow_kind\":" ++ jsonString witness.preWorkflowKind ++ ","
    ++ "\"pre_workflow_session\":" ++ jsonNatOption witness.preWorkflowSession ++ ","
    ++ "\"pre_workflow_request\":" ++ jsonNatOption witness.preWorkflowRequest ++ ","
    ++ "\"post_workflow_kind\":" ++ jsonString witness.postWorkflowKind ++ ","
    ++ "\"post_workflow_session\":" ++ jsonNatOption witness.postWorkflowSession ++ ","
    ++ "\"post_workflow_request\":" ++ jsonNatOption witness.postWorkflowRequest ++ ","
    ++ "\"selection_preserved\":" ++ boolJson witness.selectionPreserved ++ ","
    ++ "\"workflow_advanced\":" ++ boolJson witness.workflowAdvanced ++ ","
    ++ "\"transport_noop\":" ++ boolJson witness.transportNoop ++ ","
    ++ "\"can_submit_before\":" ++ boolJson witness.canSubmitBefore ++ ","
    ++ "\"can_submit_after\":" ++ boolJson witness.canSubmitAfter ++ ","
    ++ "\"selection_health\":" ++ jsonString witness.selectionHealth ++ ","
    ++ "\"projection_turn_state\":" ++ jsonStringOption witness.projectionTurnState ++ ","
    ++ "\"projection_workflow_kind\":" ++ jsonString witness.projectionWorkflowKind ++ ","
    ++ "\"projection_workflow_session\":" ++ jsonNatOption witness.projectionWorkflowSession ++ ","
    ++ "\"projection_workflow_request\":" ++ jsonNatOption witness.projectionWorkflowRequest ++ ","
    ++ "\"send_decision\":" ++ jsonString witness.sendDecision ++ ","
    ++ "\"send_blocked_reason\":" ++ jsonStringOption witness.sendBlockedReason ++ ","
    ++ "\"frontend_client_available\":" ++ boolJson witness.frontendClientAvailable ++ ","
    ++ "\"frontend_selected_agent_did\":" ++ jsonNatOption witness.frontendSelectedAgentDid ++ ","
    ++ "\"frontend_selected_session_id\":" ++ jsonNatOption witness.frontendSelectedSessionId ++ ","
    ++ "\"frontend_composer_non_empty\":" ++ boolJson witness.frontendComposerNonEmpty ++ ","
    ++ "\"frontend_sending\":" ++ boolJson witness.frontendSending ++ ","
    ++ "\"frontend_session_present\":" ++ boolJson witness.frontendSessionPresent ++ ","
    ++ "\"frontend_session_id\":" ++ jsonNatOption witness.frontendSessionId ++ ","
    ++ "\"frontend_session_latest_request_id\":" ++ jsonNatOption witness.frontendSessionLatestRequestId ++ ","
    ++ "\"frontend_session_turn_state\":" ++ jsonStringOption witness.frontendSessionTurnState ++ ","
    ++ "\"frontend_session_pending_request_id\":" ++ jsonNatOption witness.frontendSessionPendingRequestId ++ ","
    ++ "\"frontend_conversation_present\":" ++ boolJson witness.frontendConversationPresent ++ ","
    ++ "\"frontend_conversation_session_id\":" ++ jsonNatOption witness.frontendConversationSessionId ++ ","
    ++ "\"frontend_conversation_latest_request_id\":" ++ jsonNatOption witness.frontendConversationLatestRequestId ++ ","
    ++ "\"frontend_conversation_turn_state\":" ++ jsonStringOption witness.frontendConversationTurnState ++ ","
    ++ "\"frontend_local_workflow_kind\":" ++ jsonString witness.frontendLocalWorkflowKind ++ ","
    ++ "\"frontend_local_workflow_session\":" ++ jsonNatOption witness.frontendLocalWorkflowSession ++ ","
    ++ "\"frontend_local_workflow_request\":" ++ jsonNatOption witness.frontendLocalWorkflowRequest ++ ","
    ++ "\"frontend_local_workflow_turn_state\":" ++ jsonStringOption witness.frontendLocalWorkflowTurnState ++ ","
    ++ "\"frontend_expected_workflow_kind\":" ++ jsonString witness.frontendExpectedWorkflowKind ++ ","
    ++ "\"frontend_expected_workflow_session\":" ++ jsonNatOption witness.frontendExpectedWorkflowSession ++ ","
    ++ "\"frontend_expected_workflow_request\":" ++ jsonNatOption witness.frontendExpectedWorkflowRequest ++ ","
    ++ "\"frontend_expected_workflow_turn_state\":" ++ jsonStringOption witness.frontendExpectedWorkflowTurnState ++ ","
    ++ "\"frontend_expected_workflow_reason\":" ++ jsonStringOption witness.frontendExpectedWorkflowReason ++ ","
    ++ "\"frontend_expected_send_status\":" ++ jsonString witness.frontendExpectedSendStatus ++ ","
    ++ "\"frontend_expected_send_blocked_reason\":" ++ jsonStringOption witness.frontendExpectedSendBlockedReason ++ ","
    ++ "\"frontend_expected_active_request_id\":" ++ jsonNatOption witness.frontendExpectedActiveRequestId ++ ","
    ++ "\"frontend_expected_turn_state\":" ++ jsonStringOption witness.frontendExpectedTurnState ++ ","
    ++ "\"desktop_selected_session_id\":" ++ jsonNatOption witness.desktopSelectedSessionId ++ ","
    ++ "\"desktop_snapshot_present\":" ++ boolJson witness.desktopSnapshotPresent ++ ","
    ++ "\"desktop_preferred_request_id\":" ++ jsonNatOption witness.desktopPreferredRequestId ++ ","
    ++ "\"desktop_observed_request_id\":" ++ jsonNatOption witness.desktopObservedRequestId ++ ","
    ++ "\"desktop_observed_turn_state\":" ++ jsonStringOption witness.desktopObservedTurnState ++ ","
    ++ "\"desktop_expected_latest_request_id\":" ++ jsonNatOption witness.desktopExpectedLatestRequestId ++ ","
    ++ "\"desktop_expected_turn_state\":" ++ jsonStringOption witness.desktopExpectedTurnState ++ ","
    ++ "\"desktop_expect_pending_turn\":" ++ jsonBoolOption witness.desktopExpectPendingTurn
    ++ "}"

def frontendClientShellCases : List ClientShellContractCase :=
  clientShellCases

def frontendClientShellCasesJson : String :=
  jsonArray (frontendClientShellCases.map ClientShellContractCase.toJson)

def frontendClientShellCaseCount : Nat :=
  frontendClientShellCases.length

def desktopClientShellCases : List ClientShellContractCase :=
  clientShellCases.filter (fun witness => witness.desktopSelectedSessionId.isSome)

def desktopClientShellCasesJson : String :=
  jsonArray (desktopClientShellCases.map ClientShellContractCase.toJson)

def desktopClientShellCaseCount : Nat :=
  desktopClientShellCases.length

end Conformance.ClientShellContracts
