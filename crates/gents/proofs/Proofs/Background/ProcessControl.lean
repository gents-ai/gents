import Proofs.Background.State

/-!
# Background Process Control

Authorization and bounded-wait semantics for ordinary background tool calls.
A process handle remains manageable across request turns in the same session,
but never crosses the session, agent-principal, or requester-principal boundary.
The originating request remains authorized so legacy rows without requester
lineage are still manageable.
-/

namespace Subagent.ProcessControl

structure Scope where
  requestId : String
  sessionId : String
  agentDid : String
  requesterDid : Option String
  deriving DecidableEq, Repr

def authorized (caller owner : Scope) : Bool :=
  caller.sessionId == owner.sessionId &&
  caller.agentDid == owner.agentDid &&
  (caller.requestId == owner.requestId ||
    caller.requesterDid == owner.requesterDid)

theorem owner_authorized (owner : Scope) : authorized owner owner = true := by
  simp [authorized]

theorem same_principal_next_request_authorized
    (owner : Scope) (nextRequestId : String) :
    authorized { owner with requestId := nextRequestId } owner = true := by
  simp [authorized]

theorem different_session_denied
    (caller owner : Scope) (h : caller.sessionId ≠ owner.sessionId) :
    authorized caller owner = false := by
  simp [authorized, h]

theorem different_agent_denied
    (caller owner : Scope)
    (hSession : caller.sessionId = owner.sessionId)
    (hAgent : caller.agentDid ≠ owner.agentDid) :
    authorized caller owner = false := by
  simp [authorized, hSession, hAgent]

theorem different_requester_denied
    (caller owner : Scope)
    (hSession : caller.sessionId = owner.sessionId)
    (hAgent : caller.agentDid = owner.agentDid)
    (hRequest : caller.requestId ≠ owner.requestId)
    (hRequester : caller.requesterDid ≠ owner.requesterDid) :
    authorized caller owner = false := by
  simp [authorized, hSession, hAgent, hRequest, hRequester]

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
  processState : Subagent.ChildTerminal
  cancellationRequested : Bool
  reason : String
  deriving DecidableEq, Repr

def observeBoundary
    (processState : Subagent.ChildTerminal)
    (boundary : WaitBoundary) : WaitObservation :=
  { processState := processState
  , cancellationRequested := false
  , reason := boundary.reason
  }

theorem wait_boundary_preserves_process
    (state : Subagent.ChildTerminal) (boundary : WaitBoundary) :
    (observeBoundary state boundary).processState = state := by
  rfl

theorem wait_boundary_never_cancels
    (state : Subagent.ChildTerminal) (boundary : WaitBoundary) :
    (observeBoundary state boundary).cancellationRequested = false := by
  rfl

end Subagent.ProcessControl
