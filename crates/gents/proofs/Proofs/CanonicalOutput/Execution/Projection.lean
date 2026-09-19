import Proofs.CanonicalOutput.Execution.State

namespace CanonicalOutput.Execution

open RequestExecutionLease

/-- Read-only guidance for a scheduled renewal attempt. `notDue` is an early
poll, `rereadDeadline` means this generation is still live but the caller's CAS
value is stale, and `lost` means the caller must stop acting as owner. The
transition remains the authoritative decision. -/
inductive RenewalEligibility where
  | due
  | notDue (dueAt : Time)
  | rereadDeadline (actual : Time)
  | lost
  deriving DecidableEq, Repr

def renewalEligibility (world : World) (generation : Generation)
    (expectedDeadline : Time) : RenewalEligibility :=
  match world.lease.lease with
  | .active owner duration deadline =>
      if owner != generation || world.lease.now ≥ deadline then .lost
      else if ¬ RequestExecutionLease.renewableLifecycle world.lease.request then .lost
      else if deadline != expectedDeadline then .rereadDeadline deadline
      else if RequestExecutionLease.renewalDue duration deadline > world.lease.now then
        .notDue (RequestExecutionLease.renewalDue duration deadline)
      else if deadline < world.lease.now + duration then .due
      else .notDue deadline
  | _ => .lost

def requestRecords (world : World) : List Segment :=
  world.segments.filter (fun record => record.coordinate.request == world.requestId)

def requestCoordinates (world : World) : List Coordinate :=
  (requestRecords world).map (fun record => record.coordinate) |>.dedup

def requestWriterGeneration? : Writer → Option Generation
  | .request generation => some generation
  | .tool _ => none

def writtenBy (generation : Generation) (record : Segment) : Bool :=
  requestWriterGeneration? record.writer == some generation

def exactIdentityAt (records : List Segment) (record : Segment) : Bool :=
  (records.filter (fun other => other.id == record.id)).all (fun other => other == record)

def sourceIdentitiesValid (records : List Segment) (coordinate : Coordinate) : Bool :=
  (sourceRecords records coordinate).all (exactIdentityAt records)

def sourceData (records : List Segment) (coordinate : Coordinate) : List Segment :=
  (sourceRecords records coordinate).filter (fun record => record.flush.isSome) |>.dedup

def timestampsNondecreasing (records : List Segment) : Bool :=
  records.all fun left => records.all fun right =>
    match left.flush, right.flush with
    | some a, some b => if a.ordinal ≤ b.ordinal then left.createdAt ≤ right.createdAt else true
    | _, _ => true

def validateOpenPrefix (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) : Except IntegrityError Unit := do
  let data := sourceData records coordinate
  if sourceIdentitiesValid records coordinate = false then
    .error .identityConflict
  else if data.all (fun record => record.writer == writer) = false then
    .error (.invalidWriter coordinate)
  else if timestampsNondecreasing data = false then
    .error (.malformedSource coordinate)
  else
    let flushes ← (List.range data.length).mapM fun ordinal =>
      match flushAt data writer ordinal with
      | .ok flush => .ok flush
      | .error _ => .error (.malformedSource coordinate)
    match consumeFlushes flushes [] with
    | .ok _ => .ok ()
    | .error _ => .error (.malformedSource coordinate)

def headerCoordinateConflict (world : World) (message : MessageEnvelope) : Bool :=
  world.messages.any fun other => other != message &&
    (other.header.id == message.header.id ||
      (other.header.session == message.header.session && other.key == message.key) ||
      (other.header.session == message.header.session && other.sequence == message.sequence))

def segmentIdentityCollision (world : World) (record : Segment) : Bool :=
  world.segments.any (fun old => old.id == record.id && old != record)

def messageIdentityCollision (world : World) (message : MessageEnvelope) : Bool :=
  headerCoordinateConflict world message

def sourceOpen (world : World) (coordinate : Coordinate) : Bool :=
  (closures world.segments coordinate).isEmpty

def refsValid (segments : List Segment) : List PayloadRef → Bool
  | [] => true
  | ref :: rest =>
      (match reconstructPayload segments noDeniedDocuments ref with
       | .ok _ => true
       | .error _ => false) && refsValid segments rest

def completeHeaderValid (world : World) (generation : Generation)
    (segments : List Segment) (header : Header) : Bool :=
  header.request == some world.requestId && header.session == world.sessionId &&
    header.role == .assistant && header.outcome == .complete &&
    header.publication == .requestExecution generation && refsValid segments header.refs

def messageTurn (message : MessageEnvelope) : Transcript.AssistantTurn :=
  { sessionId := message.header.session
  , sequence := message.sequence
  , callIds := (toolIntents message).map (fun intent => intent.call)
  , outcome := message.header.outcome }

def acceptedMessageValid (world : World) (generation : Generation)
    (segments : List Segment) (message : MessageEnvelope) : Bool :=
  completeHeaderValid world generation segments message.header &&
    match reconstructMessage segments noDeniedDocuments message with
    | .ok _ => true
    | .error _ => false

/-- A provider acceptance cannot smuggle in an unrelated request-owned source.
All payload-bearing blocks in this provider message name the closure committed
by the same transaction. Empty messages remain valid for an empty Complete
source. Authored and tool-delivery messages use their separate publication
owners; they are not admitted through this transition. -/
def acceptedSourceBound (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) : Bool :=
  closing.coordinate.request == world.requestId &&
    (match closing.coordinate.source with | .provider _ _ _ => true | _ => false) &&
    closing.writer == .request generation &&
    message.header.refs.all (fun ref => ref.closeId == closing.id)

def authoredMessageValid (world : World) (generation : Generation)
    (segments : List Segment) (closing : Segment) (message : MessageEnvelope) : Bool :=
  message.header.request == some world.requestId &&
    message.header.session == world.sessionId && message.header.outcome == .complete &&
    message.header.role != .assistant &&
    message.header.publication == .requestExecution generation &&
    (match closing.coordinate.source with | .authored _ => true | _ => false) &&
    closing.writer == .request generation &&
    message.header.refs.all (fun ref => ref.closeId == closing.id) &&
    match reconstructMessage segments noDeniedDocuments message with
    | .ok _ => true
    | .error _ => false

def headerOnlyMessageValid (world : World) (generation : Generation)
    (message : MessageEnvelope) : Bool :=
  message.header.request == some world.requestId &&
    message.header.session == world.sessionId && message.header.outcome == .complete &&
    message.header.publication == .requestExecution generation &&
    message.header.refs.isEmpty && message.blocks.isEmpty &&
    match reconstructMessage world.segments noDeniedDocuments message with
    | .ok _ => true
    | .error _ => false

def targetIntent (message : MessageEnvelope) (call : DocId) : Option ToolIntent :=
  match (toolIntents message).filter (fun intent => intent.call == call) with
  | [intent] => some intent
  | _ => none

/-- `RemoteTarget.call` and `ToolIntent.call` are the exact physical pending
tool document identity carried by the typed block. `providerId` remains native
provider metadata and never substitutes for this document key. The authenticated
execution principal must be the coordinator, and `prepareDelegatedCall` enforces
that only a distinct remote target receives copied argument bytes. -/
def prepareDelegatedCalls (world : World) (segments : List Segment) (message : MessageEnvelope) :
    List RemoteTarget → Except Error (List DelegatedCall)
  | [] => .ok []
  | target :: rest => do
      let intent ← match targetIntent message target.call with
        | none => .error .invalidDelegation
        | some intent => .ok intent
      if target.coordinator != world.principal then .error .invalidDelegation
      let row ← match prepareDelegatedCall segments noDeniedDocuments message intent
          target.coordinator target.target with
        | .error _ => .error .invalidDelegation
        | .ok row => .ok row
      let later ← prepareDelegatedCalls world segments message rest
      .ok (row :: later)

def remoteTargetsMatchConfiguredRoutes (world : World) (message : MessageEnvelope)
    (targets : List RemoteTarget) : Bool :=
  targets.map (fun target => (target.call, target.target)) == world.remoteRoutes &&
    targets.all (fun target => target.coordinator == world.principal) &&
    world.remoteRoutes.all (fun route =>
      (toolIntents message).any (fun intent => intent.call == route.1))

def indexedDeclarations : Streams → Nat → List (Nat × Declaration)
  | [], _ => []
  | stream :: rest, index => (index, stream.1) :: indexedDeclarations rest (index + 1)

def recoveryRefs (closingId : DocId) (streams : Streams) :
    Except ExtentError (List PayloadRef) := do
  let text ← recoveryText (indexedDeclarations streams 0)
  .ok (text.map fun entry => ⟨closingId, entry.1⟩)

def recoveryTextBlock : MessageBlock PayloadSpec → Bool
  | .text payload => payload.presentation == .full
  | _ => false

def recoveryMessageValid (world : World) (generation : Generation)
    (segments : List Segment) (closing : Segment) (streams : Streams)
    (message : Option MessageEnvelope) : Bool :=
  match recoveryRefs closing.id streams with
  | .error _ => false
  | .ok expected =>
      match expected, message with
      | [], none => true
      | _ :: _, some value =>
          value.header.request == some world.requestId &&
            value.header.session == world.sessionId && value.header.role == .assistant &&
            value.header.outcome == .«partial» && value.nativeId.isNone &&
            value.header.publication == .requestRecovery generation &&
            value.header.refs == expected && envelopeRefs value == expected &&
            value.blocks.all recoveryTextBlock &&
            match reconstructMessage segments noDeniedDocuments value with
            | .ok _ => true
            | .error _ => false
      | _, _ => false

def eligibleOwnedAssistantExists (world : World) : Bool :=
  world.messages.any fun message =>
    message.header.request == some world.requestId &&
      message.header.session == world.sessionId && message.header.role == .assistant &&
      terminalPublicationEligible message.header.publication &&
      match reconstructMessage world.segments noDeniedDocuments message with
      | .ok _ => true
      | .error _ => false

def terminalSelectionValid (world : World) (selection : TerminalSelection) : Bool :=
  match selection with
  | .noMessage => !eligibleOwnedAssistantExists world
  | .message _ => match resolveTerminal (world.messages.map (fun message => message.header))
      noDeniedDocuments world.requestId world.sessionId
      (some selection) with
    | .error _ => false
    | .ok none => false
    | .ok (some header) =>
        match (world.messages.filter (fun message => message.header == header)).dedup with
        | [message] => match reconstructMessage world.segments noDeniedDocuments message with
          | .ok _ => true
          | .error _ => false
        | _ => false

end CanonicalOutput.Execution
