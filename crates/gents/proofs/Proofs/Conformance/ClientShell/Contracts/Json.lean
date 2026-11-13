import Proofs.Conformance.ClientShell.Contracts.Cases

namespace Conformance.ClientShellContracts

open Conformance.Contracts

def jsonNatList (values : List Nat) : String :=
  "[" ++ String.intercalate "," (values.map toString) ++ "]"

def ClientShellContractCase.toJson (witness : ClientShellContractCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"property\":" ++ jsonString witness.property ++ ","
    ++ "\"input\":" ++ jsonString witness.input ++ ","
    ++ "\"pre_selection_node\":" ++ jsonNatOption witness.preSelectionNode ++ ","
    ++ "\"pre_selection_session\":" ++ jsonNatOption witness.preSelectionSession ++ ","
    ++ "\"post_selection_node\":" ++ jsonNatOption witness.postSelectionNode ++ ","
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
    ++ "\"frontend_selected_node_did\":" ++ jsonNatOption witness.frontendSelectedNodeDid ++ ","
    ++ "\"frontend_selected_session_id\":" ++ jsonNatOption witness.frontendSelectedSessionId ++ ","
    ++ "\"frontend_sending\":" ++ boolJson witness.frontendSending ++ ","
    ++ "\"frontend_session_present\":" ++ boolJson witness.frontendSessionPresent ++ ","
    ++ "\"frontend_session_id\":" ++ jsonNatOption witness.frontendSessionId ++ ","
    ++ "\"frontend_session_latest_request_id\":" ++ jsonNatOption witness.frontendSessionLatestRequestId ++ ","
    ++ "\"frontend_session_turn_state\":" ++ jsonStringOption witness.frontendSessionTurnState ++ ","
    ++ "\"frontend_session_pending_request_id\":" ++ jsonNatOption witness.frontendSessionPendingRequestId ++ ","
    ++ "\"frontend_session_queued_request_ids\":" ++ jsonNatList witness.frontendSessionQueuedRequestIds ++ ","
    ++ "\"frontend_session_pending_queue_request_ids\":" ++ jsonNatList witness.frontendSessionPendingQueueRequestIds ++ ","
    ++ "\"frontend_session_folded_request_ids\":" ++ jsonNatList witness.frontendSessionFoldedRequestIds ++ ","
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
    ++ "\"desktop_queued_request_ids\":" ++ jsonNatList witness.desktopQueuedRequestIds ++ ","
    ++ "\"desktop_folded_request_ids\":" ++ jsonNatList witness.desktopFoldedRequestIds ++ ","
    ++ "\"desktop_rows\":" ++ SessionTurnCases.rowsJson witness.desktopRows ++ ","
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

def turnOfName (name : Option String) : Option ClientTurnState :=
  name.bind fun name =>
    [ClientTurnState.waitingForClaim, .running, .completed, .failed, .superseded, .interrupted].find?
      (clientTurnStateName · == name)

structure DesktopInput where
  name : String
  desktopSelectedSessionId : Nat
  desktopPreferredRequestId : Option Nat
  desktopObservedRequestId : Option Nat
  desktopObservedTurnState : Option String
  desktopRows : List ClientShell.SessionTurn.Row
  deriving Lean.ToJson

structure DesktopOutput where
  desktopSnapshotPresent : Bool
  desktopExpectedLatestRequestId : Option Nat
  desktopExpectedTurnState : Option String
  desktopQueuedRequestIds : List Nat
  desktopFoldedRequestIds : List Nat
  desktopExpectPendingTurn : Option Bool
  deriving Lean.ToJson, BEq

/-- The store a desktop snapshot reads: the session turn rows when the case
has them, otherwise the one observed request. -/
def desktopObservation (i : DesktopInput) : Option SessionObservation :=
  match i.desktopRows.getLast? with
  | some newest =>
      some (ClientShell.SessionTurn.observe i.desktopSelectedSessionId contractNode
        (some contractAgent) i.desktopRows newest)
  | none =>
      i.desktopObservedRequestId.map fun request =>
        sessionObs i.desktopSelectedSessionId (some request) (turnOfName i.desktopObservedTurnState)

def desktopProjection (obs : Option SessionObservation) : DesktopOutput :=
  { desktopSnapshotPresent := obs.isSome
    desktopExpectedLatestRequestId := obs.bind (·.latestObservedRequest)
    desktopExpectedTurnState := turnStateOptionName (obs.bind (·.latestTurn))
    desktopQueuedRequestIds := queuedRequestsOf obs
    desktopFoldedRequestIds := foldedRequestsOf obs
    desktopExpectPendingTurn := desktopPendingExpectation obs }

def desktopInputOf (c : ClientShellContractCase) : DesktopInput :=
  { name := c.name
    desktopSelectedSessionId := c.desktopSelectedSessionId.getD 0
    desktopPreferredRequestId := c.desktopPreferredRequestId
    desktopObservedRequestId := c.desktopObservedRequestId
    desktopObservedTurnState := c.desktopObservedTurnState
    desktopRows := c.desktopRows }

@[conformance_family]
def desktop : Family DesktopInput DesktopOutput where
  inputs := desktopClientShellCases.map desktopInputOf
  model := fun i => desktopProjection (desktopObservation i)

example : desktopClientShellCases.all fun c =>
    desktop.model (desktopInputOf c) ==
      { desktopSnapshotPresent := c.desktopSnapshotPresent
        desktopExpectedLatestRequestId := c.desktopExpectedLatestRequestId
        desktopExpectedTurnState := c.desktopExpectedTurnState
        desktopQueuedRequestIds := c.desktopQueuedRequestIds
        desktopFoldedRequestIds := c.desktopFoldedRequestIds
        desktopExpectPendingTurn := c.desktopExpectPendingTurn } := by native_decide

example : desktopClientShellCases.any (!·.desktopQueuedRequestIds.isEmpty) := by native_decide

example : desktopClientShellCases.any (!·.desktopFoldedRequestIds.isEmpty) := by native_decide

example : desktopClientShellCases.any fun c =>
    c.desktopObservedTurnState == some "waitingForClaim" || c.desktopObservedTurnState == some "running" := by
  native_decide

example : desktopClientShellCases.any fun c =>
    c.desktopObservedTurnState.isSome &&
      !(c.desktopObservedTurnState == some "waitingForClaim" || c.desktopObservedTurnState == some "running") := by
  native_decide

/-- The operator UI shows the observed request lifecycle: whenever a case
observes a turn, the desktop projection reports exactly that turn state. -/
example : desktopClientShellCases.all fun c =>
    c.desktopObservedTurnState.isNone ||
      (desktop.model (desktopInputOf c)).desktopExpectedTurnState == c.desktopObservedTurnState := by
  native_decide

end Conformance.ClientShellContracts
