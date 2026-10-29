import Proofs.ToolExecution.State
namespace Background.ProcessControl

structure Scope where
  requestId : String
  sessionId : String
  nodeDid : String
  requesterDid : Option String
  deriving DecidableEq, Repr

def authorized (caller owner : Scope) : Bool :=
  caller.sessionId == owner.sessionId &&
  caller.nodeDid == owner.nodeDid &&
  caller.requesterDid == owner.requesterDid

theorem owner_authorized (owner : Scope) : authorized owner owner = true := by
  simp [authorized]

theorem same_node_next_request_authorized
    (owner : Scope) (nextRequestId : String) :
    authorized { owner with requestId := nextRequestId } owner = true := by
  simp [authorized]

theorem empty_requester_does_not_alias_absent
    (requestId nextRequestId sessionId nodeDid : String) :
    authorized
      { requestId := nextRequestId, sessionId, nodeDid, requesterDid := some "" }
      { requestId, sessionId, nodeDid, requesterDid := none } = false := by
  simp [authorized]

theorem different_session_denied
    (caller owner : Scope) (h : caller.sessionId ≠ owner.sessionId) :
    authorized caller owner = false := by
  simp [authorized, h]

theorem different_agent_denied
    (caller owner : Scope)
    (hAgent : caller.nodeDid ≠ owner.nodeDid) :
    authorized caller owner = false := by
  simp [authorized, hAgent]

theorem different_requester_denied
    (caller owner : Scope)
    (hRequester : caller.requesterDid ≠ owner.requesterDid) :
    authorized caller owner = false := by
  simp [authorized, hRequester]

/-- Provider call IDs are scoped labels and may repeat across requests or
sessions. Process ownership is bound to the physical tool document as well. -/
structure Invocation where
  toolCallId : String
  toolCallDocId : String
  deriving DecidableEq, Repr

def sameInvocation (requested owned : Invocation) : Bool :=
  requested.toolCallId == owned.toolCallId && requested.toolCallDocId == owned.toolCallDocId

theorem same_label_different_document_cannot_cancel (id requestedDoc ownedDoc : String)
    (different : requestedDoc ≠ ownedDoc) :
    sameInvocation ⟨id, requestedDoc⟩ ⟨id, ownedDoc⟩ = false := by
  simp [sameInvocation, different]

theorem invocation_identity_requires_both (requested owned : Invocation) :
    sameInvocation requested owned = true ↔ requested = owned := by
  cases requested
  cases owned
  simp [sameInvocation]

/-- DefraDB authenticates the actor DID. A runtime operator controls only
physical invocations owned by that runtime; a desktop replica is not its owner. -/
def operatorAtNode (executing requested owner : String) (actor : Option String) : Bool :=
  actor == some executing && requested == executing && owner == executing

theorem operator_control_never_crosses_runtime (executing requested owner : String)
    (actor : Option String) (allowed : operatorAtNode executing requested owner actor = true) :
    actor = some executing ∧ requested = executing ∧ owner = executing := by
  simpa [operatorAtNode, Bool.and_eq_true, and_assoc] using allowed

inductive DesktopStopDecision where
  | refuse
  | alreadyTerminal
  | requestStop
  deriving DecidableEq, Repr

/-- The database authenticates and authorizes the desktop actor before its
write reaches this decision. A stop names one physical background process.
Cancellation may have been committed before the process was signalled, so a
cancelled invocation must still reach the process owner for recovery. -/
def desktopStop (authorizedActor background : Bool)
    (requested owned : Invocation) (state : ToolExecution.ToolCallState) : DesktopStopDecision :=
  if !authorizedActor || !background || !sameInvocation requested owned then .refuse
  else if state = .pending ∨ state = .running ∨ state = .cancelled then .requestStop
  else .alreadyTerminal

theorem desktop_stop_requires_authority (background : Bool) (requested owned : Invocation)
    (state : ToolExecution.ToolCallState) :
    desktopStop false background requested owned state = .refuse := by
  simp [desktopStop]

theorem desktop_stop_requires_exact_invocation (authorizedActor background : Bool)
    (requested owned : Invocation) (state : ToolExecution.ToolCallState)
    (different : requested ≠ owned) :
    desktopStop authorizedActor background requested owned state = .refuse := by
  have h : sameInvocation requested owned = false := by
    cases same : sameInvocation requested owned with
    | false => rfl
    | true => exact False.elim (different (invocation_identity_requires_both requested owned |>.mp same))
  simp [desktopStop, h]

theorem desktop_stop_preserves_completed (authorizedActor background : Bool)
    (requested owned : Invocation) :
    desktopStop authorizedActor background requested owned .completed ≠ .requestStop := by
  unfold desktopStop
  split <;> simp

theorem desktop_stop_requests_live_invocation (invocation : Invocation)
    (state : ToolExecution.ToolCallState) (live : state = .pending ∨ state = .running) :
    desktopStop true true invocation invocation state = .requestStop := by
  rcases live with rfl | rfl <;> simp [desktopStop, sameInvocation]

theorem desktop_stop_recovers_cancelled_invocation (invocation : Invocation) :
    desktopStop true true invocation invocation .cancelled = .requestStop := by
  simp [desktopStop, sameInvocation]

/-- The process owner rechecks the lifecycle when consuming an accepted
intent. Completion may win after the desktop's initial observation. -/
def consumeDesktopStop (state : ToolExecution.ToolCallState) : Bool :=
  decide (state = .pending ∨ state = .running ∨ state = .cancelled)

theorem desktop_stop_does_not_cancel_a_completed_process :
    consumeDesktopStop .completed = false := by
  rfl

theorem desktop_stop_is_consumed_for_running_process :
    consumeDesktopStop .running = true := by
  rfl

theorem desktop_stop_recovers_a_persisted_cancellation :
    consumeDesktopStop .cancelled = true := by
  rfl

inductive StopIntentPhase where
  | accepted
  | settled
  deriving DecidableEq, Repr

/-- A committed accepted intent remains pending across a runtime restart;
only a recorded terminal outcome settles it. -/
def stopNeedsExecution (phase : StopIntentPhase) : Bool :=
  decide (phase = .accepted)

theorem accepted_stop_is_recovered : stopNeedsExecution .accepted = true := by
  rfl

theorem settled_stop_is_not_reexecuted : stopNeedsExecution .settled = false := by
  rfl

inductive WaitBoundary where
  | waitTimeout
  | callerInterrupted
  | callerDeadline
  deriving DecidableEq, Repr

namespace WaitBoundary

def reason : WaitBoundary -> String
  | .waitTimeout => "wait_timeout"
  | .callerInterrupted => "caller_interrupted"
  | .callerDeadline => "caller_deadline_exceeded"

end WaitBoundary

structure WaitObservation where
  processState : ToolExecution.ToolCallState
  cancellationRequested : Bool
  reason : String
  deriving DecidableEq, Repr

def observeBoundary
    (processState : ToolExecution.ToolCallState)
    (boundary : WaitBoundary) : WaitObservation :=
  { processState := processState
  , cancellationRequested := false
  , reason := boundary.reason
  }

theorem wait_boundary_preserves_process
    (state : ToolExecution.ToolCallState) (boundary : WaitBoundary) :
    (observeBoundary state boundary).processState = state := by
  rfl

theorem wait_boundary_never_cancels
    (state : ToolExecution.ToolCallState) (boundary : WaitBoundary) :
    (observeBoundary state boundary).cancellationRequested = false := by
  rfl

end Background.ProcessControl
