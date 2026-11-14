import Proofs.Conformance.ClientShell.Contracts.Cases

namespace Conformance.ClientShellContracts

open Conformance.Contracts

def desktopClientShellCases : List ClientShellContractCase :=
  clientShellCases.filter (fun witness => witness.desktopSelectedSessionId.isSome)

def turnOfName (name : Option String) : Option ClientTurnState :=
  name.bind fun name =>
    [ClientTurnState.waitingForClaim, .running, .completed, .failed, .superseded, .interrupted].find?
      (clientTurnStateName · == name)

structure FrontendInput where
  name : String
  frontendClientAvailable : Bool
  frontendSelectedNodeDid : Option Nat
  frontendSelectedSessionId : Option Nat
  frontendSending : Bool
  frontendSessionPresent : Bool
  frontendSessionId : Option Nat
  frontendSessionLatestRequestId : Option Nat
  frontendSessionTurnState : Option String
  frontendSessionPendingRequestId : Option Nat
  frontendSessionQueuedRequestIds : List Nat
  frontendSessionPendingQueueRequestIds : List Nat
  frontendSessionFoldedRequestIds : List Nat
  frontendLocalWorkflowKind : String
  frontendLocalWorkflowSession : Option Nat
  frontendLocalWorkflowRequest : Option Nat
  frontendLocalWorkflowTurnState : Option String
  deriving Lean.ToJson

structure FrontendOutput where
  frontendExpectedWorkflowKind : String
  frontendExpectedWorkflowSession : Option Nat
  frontendExpectedWorkflowRequest : Option Nat
  frontendExpectedWorkflowTurnState : Option String
  frontendExpectedWorkflowReason : Option String
  frontendExpectedSendStatus : String
  frontendExpectedSendBlockedReason : Option String
  frontendExpectedActiveRequestId : Option Nat
  frontendExpectedTurnState : Option String
  deriving Lean.ToJson, BEq

def frontendInputOf (c : ClientShellContractCase) : FrontendInput :=
  { name := c.name
    frontendClientAvailable := c.frontendClientAvailable
    frontendSelectedNodeDid := c.frontendSelectedNodeDid
    frontendSelectedSessionId := c.frontendSelectedSessionId
    frontendSending := c.frontendSending
    frontendSessionPresent := c.frontendSessionPresent
    frontendSessionId := c.frontendSessionId
    frontendSessionLatestRequestId := c.frontendSessionLatestRequestId
    frontendSessionTurnState := c.frontendSessionTurnState
    frontendSessionPendingRequestId := c.frontendSessionPendingRequestId
    frontendSessionQueuedRequestIds := c.frontendSessionQueuedRequestIds
    frontendSessionPendingQueueRequestIds := c.frontendSessionPendingQueueRequestIds
    frontendSessionFoldedRequestIds := c.frontendSessionFoldedRequestIds
    frontendLocalWorkflowKind := c.frontendLocalWorkflowKind
    frontendLocalWorkflowSession := c.frontendLocalWorkflowSession
    frontendLocalWorkflowRequest := c.frontendLocalWorkflowRequest
    frontendLocalWorkflowTurnState := c.frontendLocalWorkflowTurnState }

/-- The one session snapshot the frontend holds: the selected session as the
client observed it, owned by the selected node and the contract agent. -/
def frontendStore (i : FrontendInput) : LocalStore :=
  storeWith (if i.frontendSessionPresent then
    [{ sessionId := i.frontendSessionId.getD 0
       nodeDid := i.frontendSelectedNodeDid.getD contractNode
       agentDid := some contractAgent
       latestObservedRequest := i.frontendSessionLatestRequestId
       latestTurn := turnOfName i.frontendSessionTurnState
       queuedRequests := i.frontendSessionQueuedRequestIds
       pendingQueueRequests := i.frontendSessionPendingQueueRequestIds
       foldedRequests := i.frontendSessionFoldedRequestIds }]
  else [])

/-- The frontend's own workflow as a shell workflow. A turn already in
progress is the idle workflow over a non-terminal observed turn; the client
keeps no blocked workflow without its reason, so no case starts blocked. -/
def frontendWorkflow (i : FrontendInput) : SubmissionWorkflow :=
  match i.frontendLocalWorkflowKind with
  | "submittingRequest" =>
      .submitting (i.frontendSelectedNodeDid.getD contractNode) i.frontendLocalWorkflowSession
  | "awaitingObservation" =>
      .awaiting (i.frontendLocalWorkflowSession.getD 0) (i.frontendLocalWorkflowRequest.getD 0)
  | _ => .idle

def frontendShell (i : FrontendInput) : ShellState :=
  { selection :=
      { peer := some contractPeer
        node := i.frontendSelectedNodeDid
        session := i.frontendSelectedSessionId }
    workflow := frontendWorkflow i }

/-- The frontend reconciles its workflow with the selection and the snapshot
it holds: selecting a session drops a workflow for another session
(`workflowAfterSelectSession`), and the snapshot retires an awaited request it
observes (`snapshotAdvanceWorkflow`). -/
def frontendReconciled (shell : ShellState) (store : LocalStore) : ShellState :=
  let selected :=
    match shell.selection.session with
    | some sid => workflowAfterSelectSession sid shell.workflow
    | none => shell.workflow
  { shell with workflow := snapshotAdvanceWorkflow selected store }

def frontendProjection (i : FrontendInput) : FrontendOutput :=
  let store := frontendStore i
  let ctx : SubmitContext :=
    { clientAvailable := i.frontendClientAvailable, requestedAgent := some contractAgent }
  let shell := frontendShell i
  let reconciled := frontendReconciled shell store
  let chat := projectChat reconciled store ctx
  let obs := selectedObservation shell store
  let active := activeRequestForFrontend shell.selection obs shell.workflow
  let workflow := frontendWorkflowFromProjection reconciled store ctx active
  { frontendExpectedWorkflowKind := workflow.kind
    frontendExpectedWorkflowSession := workflow.sessionId
    frontendExpectedWorkflowRequest := workflow.requestId
    frontendExpectedWorkflowTurnState := workflow.turnState
    frontendExpectedWorkflowReason := workflow.reason
    frontendExpectedSendStatus := frontendSendStatus chat.sendDecision
    frontendExpectedSendBlockedReason := frontendSendReason chat.sendDecision
    frontendExpectedActiveRequestId := active
    frontendExpectedTurnState := turnStateOptionName (obs.bind (·.latestTurn)) }

@[conformance_family]
def frontend : Family FrontendInput FrontendOutput where
  inputs := clientShellCases.map frontendInputOf
  model := frontendProjection

/-- The frontend projection agrees with every stepped ClientShell scenario. -/
example : clientShellCases.all fun c =>
    frontend.model (frontendInputOf c) ==
      { frontendExpectedWorkflowKind := c.frontendExpectedWorkflowKind
        frontendExpectedWorkflowSession := c.frontendExpectedWorkflowSession
        frontendExpectedWorkflowRequest := c.frontendExpectedWorkflowRequest
        frontendExpectedWorkflowTurnState := c.frontendExpectedWorkflowTurnState
        frontendExpectedWorkflowReason := c.frontendExpectedWorkflowReason
        frontendExpectedSendStatus := c.frontendExpectedSendStatus
        frontendExpectedSendBlockedReason := c.frontendExpectedSendBlockedReason
        frontendExpectedActiveRequestId := c.frontendExpectedActiveRequestId
        frontendExpectedTurnState := c.frontendExpectedTurnState } := by native_decide

example : frontend.inputs.any (!·.frontendSessionFoldedRequestIds.isEmpty) := by native_decide

example : frontend.inputs.any (!·.frontendSessionQueuedRequestIds.isEmpty) := by native_decide

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
