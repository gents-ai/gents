import Proofs.CanonicalOutput.Delegation
import Proofs.RequestExecutionLease
import Proofs.Transcript
import Proofs.ToolExecution.Executable
import Proofs.Session.State
import Proofs.Goals
import Proofs.StorageWriteGate
import Proofs.Background.CompletionContinuation
import Proofs.CompletionRetry.State

/-!
# Canonical output execution composition

This layer composes immutable output records, the request lease owner, terminal
selection, and transcript/tool-intent publication. DefraDB transaction isolation,
ACP evidence, genesis/CID validation, and the monotonic owner clock remain native
boundaries. Every mutating transition below represents one local
`MutationWriteGate` critical section; no cross-process mutex claim is made.
-/

namespace CanonicalOutput.Execution

abbrev Generation := Nat

namespace Handover

structure PhysicalRequestAdmission where
  document : DocId
  entry : SessionQueue.QueueEntry
  agent : Nat
  session : SessionId
  requester : Option Nat
  authenticated : Bool
  subagentDepth : Nat := 0
  workspace : Option DelegatedWorkspace := none
  deriving DecidableEq, Repr

structure GoalChildReceipt where
  goalDocument : DocId
  request : PhysicalRequestAdmission
  parentPhysical : DocId
  parentLogical : RequestId
  authenticated : Bool
  deriving DecidableEq, Repr

inductive ClaimEvidence where
  | backgroundWake (snapshot : BackgroundCompletion.WakeAttemptSnapshot)
  | ordinary
  | goalChild (receipt : GoalChildReceipt)
  deriving DecidableEq, Repr

structure Activation where
  request : PhysicalRequestAdmission
  evidence : ClaimEvidence
  configuredRoutes : List (DocId × Nat × Nat)
  routesAuthenticated : Bool
  generation : Generation
  duration : Time
  deadline : Time
  deriving DecidableEq, Repr

structure ClaimedBinding where
  physicalRequest : DocId
  logicalRequest : RequestId
  session : SessionId
  evidence : ClaimEvidence
  deriving DecidableEq, Repr

end Handover

/-- A physical tool document and the existing lifecycle context that will back
the accepted call. The context's older logical identifiers are not document
authority and need not equal `document`. -/
structure ToolAdmission where
  document : DocId
  context : ToolExecution.ToolCallContext
  delegatedWorkspace : Option DelegatedWorkspace := none
  deriving DecidableEq, Repr

inductive ToolProvenance where
  | acceptedIntent
  | spawnedBackground (parentToolDoc : DocId)
  deriving DecidableEq, Repr

structure ToolGenesis where
  logicalCallId : ToolExecution.ToolCallId
  logicalRequestId : RequestId
  operation : ToolExecution.ToolOperation
  childRequestId : Option RequestId
  spawnBehaviorId : Option Nat
  deriving DecidableEq, Repr

def ToolGenesis.fromContext (context : ToolExecution.ToolCallContext) : ToolGenesis :=
  { logicalCallId := context.callId
  , logicalRequestId := context.requestId
  , operation := context.operation
  , childRequestId := context.childRequestId
  , spawnBehaviorId := context.spawnBehaviorId }

/-- Exact accepted-header ownership plus projections of existing durable native
cancellation/reconciliation fields. None of these fields claims that a running
host effect has stopped. -/
structure OwnedTool where
  document : DocId
  requestDoc : DocId
  session : SessionId
  acceptedSequence : Transcript.Sequence
  provenance : ToolProvenance := .acceptedIntent
  context : ToolExecution.ToolCallContext
  delegatedWorkspace : Option DelegatedWorkspace := none
  cancelCascadeIntentAt : Option Time := none
  cancelPendingRemoteAck : Bool := false
  stuckSince : Option Time := none
  deriving DecidableEq, Repr

structure SpawnedToolAdmission where
  document : DocId
  parentToolDoc : DocId
  context : ToolExecution.ToolCallContext
  deriving DecidableEq, Repr

/-- Authenticated projection of the existing durable wake AgentRequest and
its physical document binding. Logical `entry.requestId` is never substituted
for `wakeDocument`. -/
structure WakeDocumentBinding where
  entry : SessionQueue.QueueEntry
  agent : Nat
  session : SessionId
  notificationMessageId : Transcript.MessageId
  notificationSequence : Transcript.Sequence
  wakeDocument : DocId
  authenticated : Bool
  deriving DecidableEq, Repr

/-- Authenticated observation of the canonical Goal owner selected in the
same transaction as a parent-bound completion notification. -/
structure GoalNotificationBinding where
  goalDocument : DocId
  parentRequestDocument : DocId
  agent : Nat
  session : SessionId
  status : Goals.Status
  authenticated : Bool
  deriving DecidableEq, Repr

structure World where
  requestId : DocId
  sessionId : SessionId
  /-- Authenticated coordinator principal supplied by existing DID/ACP ownership
  at the local gate. It is not inferred from a source or remote target. -/
  principal : Nat
  /-- Current accepted physical request depth, read from its owner at publication. -/
  subagentDepth : Nat := 0
  /-- Parent workspace stamp from the same authenticated request owner. -/
  workspace : Option DelegatedWorkspace := none
  /-- Exact `(physical tool document, remote target, selected behavior)` subset projected from the
  existing configured routing owner for the turn being accepted. Local calls
  are absent. This is an authenticated owner snapshot, not caller-created ACP. -/
  remoteRoutes : List (DocId × Nat × Nat)
  lease : RequestExecutionLease.World Generation
  segments : List Segment
  messages : List MessageEnvelope
  transcript : Transcript.TranscriptState
  /-- Authoritative session compaction cursor. Fresh tool-result publication
  must allocate strictly beyond it; exact replay does not allocate. -/
  compactionCursor : Option Transcript.Sequence := none
  toolContexts : List OwnedTool := []
  delegatedCalls : List DelegatedCall
  terminalSelection : Option TerminalSelection
  gateOwner : Option Nat := none
  gateSchedule : StorageWriteGate.State := ⟨.released, true, false⟩
  queue : SessionQueue.SessionQueueState :=
    { scope := { agent := 0, session := 0, requester := none }
    , active := none, pending := [], terminal := ∅ }
  claimed : Option Handover.ClaimedBinding := none
  retry : CompletionRetry.State :=
    { request := 0, scope := 0, turn := 0, phase := .issuing
    , budget := { transportRetries := 0, resampleRetries := 0, allowRepair := false }
    , transportUsed := 0, resampleUsed := 0, repairUsed := false
    , lastParseError := none, now := 0, deadline := none, attempt := 0
    , usageCharged := 0 }
  deriving DecidableEq

def World.currentGeneration? (world : World) : Option Generation :=
  match world.lease.lease with
  | .active generation _ _ => some generation
  | _ => none

/-- Routing/ACP validates these configured principals natively. This model binds
the addressed call to exact reconstructed argument bytes before constructing the
remote-only row. -/
structure RemoteTarget where
  call : DocId
  coordinator : Nat
  target : Nat
  behavior : Nat
  deriving DecidableEq, Repr

/-- Evidence projected from the existing cancellation and tool-policy owners at
the same local gate. This execution model does not duplicate either policy
machine; it makes their admission boundary explicit instead of treating a
published pending row as unconditional permission to start external work. -/
structure DispatchPermit where
  call : ToolExecution.ToolCallId
  cancellationAllows : Bool
  toolPolicyAllows : Bool
  deriving DecidableEq, Repr

inductive IntegrityError where
  | identityConflict
  | malformedSource (coordinate : Coordinate)
  | invalidWriter (coordinate : Coordinate)
  deriving DecidableEq, Repr

/-- One keyed provider source in an atomic recovery batch. `closing` may be a
previously committed, byte-identical Partial closure (lost acknowledgement); it
is never replaced. `message` is absent exactly when recovery found no Text
stream worth publishing. -/
structure RecoveryItem where
  closing : Segment
  message : Option MessageEnvelope
  deriving DecidableEq, Repr

structure RecoveryPrepared where
  segments : List Segment
  messages : List MessageEnvelope
  transcript : Transcript.TranscriptState
  deriving DecidableEq

inductive Error where
  | integrity (error : IntegrityError)
  | sourceAlreadyClosed
  | invalidSegment
  | invalidHeader
  | invalidRecoveryHeader
  | identityCollision
  | leaseRejected
  | transcriptRejected
  | terminalRejected
  | publicationIncomplete
  | invalidDelegation
  deriving DecidableEq, Repr

/-- The execution model carries no denial oracle. ACP-denied dependency behavior
is already modeled by reconstruction/hydration and remains a native input here. -/
def noDeniedDocuments : List DocId := []

end CanonicalOutput.Execution
