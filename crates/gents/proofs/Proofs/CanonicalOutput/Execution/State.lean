import Proofs.CanonicalOutput.Delegation
import Proofs.RequestExecutionLease
import Proofs.Transcript

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

structure World where
  requestId : DocId
  sessionId : SessionId
  /-- Authenticated coordinator principal supplied by existing DID/ACP ownership
  at the local gate. It is not inferred from a source or remote target. -/
  principal : Nat
  /-- Exact `(physical tool document, remote target)` subset projected from the
  existing configured routing owner for the turn being accepted. Local calls
  are absent. This is an authenticated owner snapshot, not caller-created ACP. -/
  remoteRoutes : List (DocId × Nat)
  lease : RequestExecutionLease.World Generation
  segments : List Segment
  messages : List MessageEnvelope
  transcript : Transcript.TranscriptState
  delegatedCalls : List DelegatedCall
  terminalSelection : Option TerminalSelection
  deriving DecidableEq

def World.lifecycle (world : World) : RequestState := world.lease.request

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
  | headerConflict
  | closureConflict (coordinate : Coordinate)
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
  | wrongRequest
  | wrongSession
  | wrongGeneration
  | wrongTimestamp
  | sourceAlreadyClosed
  | sourceStillOpen
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
