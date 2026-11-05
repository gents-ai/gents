import Proofs.ToolExecution.State
import Proofs.ToolExecution.Executable
import Proofs.ManagedExec.Ownership
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

def operatorProcessAuthorized
    (executing requested owner : String) (actor : Option String)
    (requestedInvocation ownedInvocation : Invocation) (background native : Bool) : Bool :=
  operatorAtNode executing requested owner actor &&
  sameInvocation requestedInvocation ownedInvocation && background && native

theorem operator_process_requires_exact_invocation
    (executing requested owner : String) (actor : Option String)
    (requestedInvocation ownedInvocation : Invocation) (background native : Bool)
    (allowed : operatorProcessAuthorized executing requested owner actor
      requestedInvocation ownedInvocation background native = true) :
    requestedInvocation = ownedInvocation := by
  have same : sameInvocation requestedInvocation ownedInvocation = true := by
    simp only [operatorProcessAuthorized, Bool.and_eq_true] at allowed
    exact allowed.1.1.2
  exact (invocation_identity_requires_both _ _).mp same

/-- A client requests job control by writing an intent with its own DID. The
agent that owns the job consumes that intent and settles the existing tool-call
lifecycle. Native ACP reads and the intent write share a transaction: a
concurrent revocation that invalidates the read must conflict before commit.
This is an external DefraDB transaction premise, not a second authorization
decision in the runtime. -/
structure ClientIntent where
  actor : String
  invocation : Invocation
  revision : Nat
  deriving DecidableEq, Repr

/-- The authenticated client signs the physical targets and operation before
publication. Native ownership and the transport actor are independent facts;
neither replaces verification of this durable signature. -/
structure SignedControlScope where
  author : String
  node : String
  agent : String
  sessionDoc : String
  requestDoc : String
  invocation : Invocation
  action : String
  deriving DecidableEq, Repr

def signedScopeAccepted (signed observed : SignedControlScope)
    (signatureValid : Bool) : Bool := signatureValid && signed == observed

theorem altered_signed_scope_is_rejected (signed observed : SignedControlScope)
    (signatureValid : Bool) (different : signed ≠ observed) :
    signedScopeAccepted signed observed signatureValid = false := by
  simp [signedScopeAccepted, different]

/-- A committed claim retains the signed scope under the selected agent's
native ownership. Recovery uses that retained evidence even if the original
client intent is no longer visible to the agent. -/
def retainedClaimMayRun (agent : String) (claimOwner : Option String)
    (retained observed : SignedControlScope) (signatureValid : Bool) : Bool :=
  claimOwner == some agent && retained.agent == agent &&
    signedScopeAccepted retained observed signatureValid

theorem retained_claim_never_uses_another_agent (agent : String) (owner : Option String)
    (retained observed : SignedControlScope) (valid : Bool) (different : owner ≠ some agent) :
    retainedClaimMayRun agent owner retained observed valid = false := by
  simp [retainedClaimMayRun, different]

def commitClientIntent (authenticatedActor : Option String) (authorized : Bool)
    (observedRevision currentRevision : Nat) (invocation : Invocation) : Option ClientIntent :=
  if authorized && observedRevision == currentRevision then
    authenticatedActor.map (fun actor => ⟨actor, invocation, currentRevision⟩)
  else none

theorem revocation_conflict_refuses_stale_intent (actor : Option String)
    (authorized : Bool) (observed current : Nat) (invocation : Invocation)
    (stale : observed ≠ current) :
    commitClientIntent actor authorized observed current invocation = none := by
  simp [commitClientIntent, stale]

theorem denied_intent_has_no_runtime_fallback (actor : Option String)
    (observed current : Nat) (invocation : Invocation) :
    commitClientIntent actor false observed current invocation = none := by
  simp [commitClientIntent]

def agentMaySettle (agent actor : String) (intent : ClientIntent)
    (ownedInvocation : Invocation) : Bool :=
  actor == agent && sameInvocation intent.invocation ownedInvocation

/-- Claiming an intent requires the selected agent to recheck the native
author's current access in the same transaction as the claim. The physical
intent owner and its authenticated author are independent native facts; neither
is inferred from a writable document field. A revocation invalidating the
transaction's observed access must conflict before the claim commits. -/
def agentMayClaim (agent claimant : String) (intent : ClientIntent)
    (nativeOwner : Option String) (ownedInvocation : Invocation)
    (mayRead mayUpdate mayInvoke : Bool) (observedRevision currentRevision : Nat) : Bool :=
  claimant == agent && nativeOwner == some intent.actor &&
    sameInvocation intent.invocation ownedInvocation && mayRead && mayUpdate && mayInvoke &&
    observedRevision == currentRevision

theorem claim_requires_current_authority (agent claimant : String) (intent : ClientIntent)
    (owner : Option String) (invocation : Invocation) (read invoke : Bool) (observed current : Nat) :
    agentMayClaim agent claimant intent owner invocation read false invoke observed current = false := by
  simp [agentMayClaim]

theorem claim_rejects_revocation_conflict (agent claimant : String) (intent : ClientIntent)
    (owner : Option String) (invocation : Invocation) (read update invoke : Bool)
    (observed current : Nat) (stale : observed ≠ current) :
    agentMayClaim agent claimant intent owner invocation read update invoke observed current = false := by
  simp [agentMayClaim, stale]

theorem claim_requires_native_author (agent claimant : String) (intent : ClientIntent)
    (owner : Option String) (invocation : Invocation) (read update invoke : Bool)
    (observed current : Nat) (different : owner ≠ some intent.actor) :
    agentMayClaim agent claimant intent owner invocation read update invoke observed current = false := by
  simp [agentMayClaim, different]

/-- A durable claim is reusable only when its native document owner is the
selected agent. Its claim transaction already checked the client's authority;
later revocation does not undo that committed decision. -/
def committedClaimMaySettle (agent : String) (claimOwner : Option String)
    (intent : ClientIntent) (ownedInvocation : Invocation) : Bool :=
  claimOwner == some agent && sameInvocation intent.invocation ownedInvocation

theorem foreign_claim_cannot_authorize_settlement (agent : String) (owner : Option String)
    (intent : ClientIntent) (invocation : Invocation) (different : owner ≠ some agent) :
    committedClaimMaySettle agent owner intent invocation = false := by
  simp [committedClaimMaySettle, different]

/-- A remotely observed outcome must be signed by the selected agent and bind
the exact physical intent, author and invocation. The native transaction also
checks that the durable claim belongs to that agent. -/
structure ControlReceipt where
  intentDoc : String
  author : String
  agent : String
  invocation : Invocation
  deriving DecidableEq, Repr

def receiptAccepted (receipt expected : ControlReceipt) (signatureValid : Bool) : Bool :=
  signatureValid && receipt == expected

theorem unsigned_control_receipt_rejected (receipt expected : ControlReceipt) :
    receiptAccepted receipt expected false = false := by
  simp [receiptAccepted]

theorem altered_control_receipt_rejected (receipt expected : ControlReceipt)
    (signatureValid : Bool) (different : receipt ≠ expected) :
    receiptAccepted receipt expected signatureValid = false := by
  simp [receiptAccepted, different]

/-- An operational failure may occur after a signal or durable write. Its
signed receipt reports uncertainty and does not assert either outcome. -/
inductive ControlResult where
  | applied | rejected | unconfirmed
  deriving DecidableEq, BEq, Repr

def resultConfirmed (result : ControlResult) : Bool := result == .applied

theorem unconfirmed_does_not_confirm_effect : resultConfirmed .unconfirmed = false := by rfl

/-- The signature authenticates the result along with the receipt's scope. -/
def signedResultMatches (observed signed : ControlResult) : Bool := observed == signed

theorem changed_result_is_not_authenticated (observed signed : ControlResult)
    (different : observed ≠ signed) : signedResultMatches observed signed = false := by
  cases observed <;> cases signed <;> simp_all [signedResultMatches] <;> decide

/-- Detached ACP grants cannot fence concurrent revocation. Receipt
publication instead rechecks current authority in the native write transaction;
unavailable or revoked authority leaves the outcome undisclosed. -/
def mayPublishOutcome (currentAuthority transactionFenced : Bool) : Bool :=
  currentAuthority && transactionFenced

theorem revoked_authority_cannot_publish_outcome (fenced : Bool) :
    mayPublishOutcome false fenced = false := by simp [mayPublishOutcome]

theorem unfenced_authority_cannot_publish_outcome (authority : Bool) :
    mayPublishOutcome authority false = false := by simp [mayPublishOutcome]

/-- A retained agent identity may reject newly submitted work while its
configuration is unavailable; it may still finish an already committed claim. -/
inductive UnavailableControlAction where
  | execute | reject
  deriving DecidableEq, BEq, Repr

def unavailableControlAction (alreadyClaimed : Bool) : UnavailableControlAction :=
  if alreadyClaimed then .execute else .reject

theorem unavailable_agent_rejects_unclaimed :
    unavailableControlAction false = .reject := by rfl

theorem unavailable_agent_finishes_claimed :
    unavailableControlAction true = .execute := by rfl

theorem settlement_requires_agent (agent actor : String) (intent : ClientIntent)
    (ownedInvocation : Invocation)
    (allowed : agentMaySettle agent actor intent ownedInvocation = true) :
    actor = agent := by
  simp only [agentMaySettle, Bool.and_eq_true, beq_iff_eq] at allowed
  exact allowed.1

/-- A stop selects the existing tool transition for the durable state it reads.
An undispatched invocation has no process to signal: its cancellation commits
first and dispatch can no longer start it. When dispatch moves the row first,
the stop re-reads it and applies to the running invocation. -/
inductive StopAction where
  | cancelBeforeDispatch
  | stopProcess
  | reportTerminal
  deriving DecidableEq, Repr

def stopAction : ToolExecution.ToolCallState → StopAction
  | .pending => .cancelBeforeDispatch
  | .running => .stopProcess
  | _ => .reportTerminal

theorem pending_stop_prevents_dispatch (c : ToolExecution.ToolCallContext)
    (cause : ToolExecution.CancelCause) (h : c.state = .pending) :
    ∃ post, c.step? (.cancelBeforeDispatch cause) = some post ∧
      post.step? .dispatch = none := by
  refine ⟨{ c with state := .cancelled }, ?_, ?_⟩ <;>
    simp [ToolExecution.ToolCallContext.step?, h]

theorem stop_never_signals_undispatched :
    stopAction .pending ≠ .stopProcess := by decide

theorem stop_reports_every_terminal (state : ToolExecution.ToolCallState)
    (h : state ≠ .pending ∧ state ≠ .running) : stopAction state = .reportTerminal := by
  cases state <;> simp_all [stopAction]

/-- A control request is not a terminal outcome. The host must first observe
the process stop; an unverified stop leaves the durable invocation running.
Lost ownership is recorded as a failure rather than a confirmed cancellation. -/
def stopSettlement : ManagedExec.StopOutcome → ToolExecution.ToolCallState
  | .stopped => .cancelled
  | .stillRunning => .running
  | .alreadyExited | .notOwned => .failed

theorem unverified_stop_preserves_running :
    stopSettlement .stillRunning = .running := by rfl

theorem cancelled_requires_confirmed_stop (outcome : ManagedExec.StopOutcome)
    (h : stopSettlement outcome = .cancelled) : outcome = .stopped := by
  cases outcome <;> simp_all [stopSettlement]

/-- Host evidence belongs to one physical invocation. A released worker that
never spawned is stopped; a spawned process requires observed exit. A missing
record is not evidence that no process was spawned. -/
inductive LiveProcessEvidence where
  | neverSpawned
  | spawnedUnverified
  | exited
  deriving DecidableEq, Repr

/-- Normal return preserves existing completion semantics. Every path that
abandons a native future must prove that its invocation never spawned or exited. -/
inductive TerminalIntent where
  | completed
  | cancelled
  | timedOut
  | failed
  | panicked
  deriving DecidableEq, Repr

def maySettleTerminal (intent : TerminalIntent) (evidence : LiveProcessEvidence) : Bool :=
  match intent with
  | .completed => true
  | _ => evidence != .spawnedUnverified

theorem abandoned_spawn_requires_exit (intent : TerminalIntent)
    (h : intent ≠ .completed) : maySettleTerminal intent .spawnedUnverified = false := by
  cases intent <;> simp_all [maySettleTerminal]

def settleLiveCancel (released : Bool) (evidence : LiveProcessEvidence) : ManagedExec.StopOutcome :=
  if !released then .stillRunning else
    match evidence with
    | .neverSpawned | .exited => .stopped
    | .spawnedUnverified => .stillRunning

theorem live_cancel_requires_release (evidence : LiveProcessEvidence) :
    settleLiveCancel false evidence = .stillRunning := by rfl

theorem missing_exit_evidence_preserves_running (released : Bool) :
    settleLiveCancel released .spawnedUnverified = .stillRunning := by
  cases released <;> rfl

theorem released_without_spawn_is_stopped :
    settleLiveCancel true .neverSpawned = .stopped := by rfl

theorem released_confirmed_exit_is_stopped :
    settleLiveCancel true .exited = .stopped := by rfl

/-- The first terminal outcome is immutable; a later stop request cannot turn
an already timed-out or failed invocation into an explicit cancellation. -/
def firstTerminalIntent (first : Option TerminalIntent) (next : TerminalIntent) : TerminalIntent :=
  first.getD next

theorem existing_terminal_intent_wins (first next : TerminalIntent) :
    firstTerminalIntent (some first) next = first := by rfl

/-- Native failure can be decided before cleanup produces the complete tool
outcome. Its reserved position is resolved only by that failure, and later
cancellation cannot displace it. -/
inductive IntentSlot where
  | empty
  | failurePending
  | decided (intent : TerminalIntent)
  deriving DecidableEq, Repr

def observeIntent (slot : IntentSlot) (intent : TerminalIntent) : IntentSlot :=
  match slot with
  | .empty => .decided intent
  | .failurePending => if intent = .failed then .decided .failed else .failurePending
  | .decided existing => .decided existing

theorem pending_failure_beats_cancel : observeIntent .failurePending .cancelled = .failurePending := by rfl

theorem pending_failure_resolves : observeIntent .failurePending .failed = .decided .failed := by rfl

/-- Confirmation remains owned until its terminal write commits, so recovery can
replay the original cause after a storage failure. -/
def retireStopReceipt (terminalCommitted : Bool) : Bool := terminalCommitted

theorem failed_terminal_write_retains_stop_receipt : retireStopReceipt false = false := by rfl

/-- A released worker may leave an owned, unsettled invocation. Completion
waiters are notified only when that retained owner is retired. -/
def ownerCompletionNotified (ownerRetained : Bool) : Bool := !ownerRetained

theorem retained_owner_is_not_completion : ownerCompletionNotified true = false := by rfl

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
