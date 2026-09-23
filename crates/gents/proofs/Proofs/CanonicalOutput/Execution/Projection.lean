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

def sourceIdentitiesValid (records : List Segment) (coordinate : Coordinate) : Bool :=
  (sourceRecords records coordinate).all (exactIdentityAt records)

def sourceData (records : List Segment) (coordinate : Coordinate) : List Segment :=
  (sourceRecords records coordinate).filter (fun record => record.flush.isSome) |>.dedup

def reconstructOpenPrefix (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) : Except IntegrityError Streams := do
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
    | .ok streams => .ok streams
    | .error _ => .error (.malformedSource coordinate)

def validateOpenPrefix (records : List Segment) (coordinate : Coordinate)
    (writer : Writer) : Except IntegrityError Unit := do
  let _ ← reconstructOpenPrefix records coordinate writer
  pure ()

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

def ownedToolByDocument? (world : World) (document : DocId) : Option OwnedTool :=
  match world.toolContexts.filter (fun tool => tool.document == document) with
  | [tool] => some tool
  | _ => none

def replaceOwnedTool (tools : List OwnedTool) (document : DocId)
    (replacement : OwnedTool) : List OwnedTool :=
  tools.map fun tool => if tool.document == document then replacement else tool

def physicalRunning (world : World) (document : DocId) : Bool :=
  match ownedToolByDocument? world document with
  | some tool => tool.context.state == .running
  | none => false

/-- Replicated argument bytes are not permission to start. The authoritative
tool document must still be pending and free of cancellation/reconcile handoff
markers at the shared-state admission boundary. Replication latency and the
native remote gate that supplies this observation are refinement premises. -/
def remoteExecutionAdmitted (world : World) (document : DocId) : Bool :=
  match ownedToolByDocument? world document with
  | some tool => tool.context.state == .pending &&
      tool.cancelCascadeIntentAt.isNone && !tool.cancelPendingRemoteAck &&
      tool.stuckSince.isNone
  | none => false

def transcriptToolByDocument? (world : World) (document : DocId) :
    Option Transcript.ToolCallRow :=
  match world.transcript.toolCalls.filter (fun row => row.callId == document) with
  | [row] => some row
  | _ => none

def toolHandedOff (tool : OwnedTool) : Bool :=
  tool.stuckSince.isSome || tool.context.awaitMode == .background

/-- Structural ownership for already-published lifecycle work. Conflicting
segment bytes do not erase ownership, but ambiguous message metadata does. -/
def directAcceptedHeaderMetadataBindsTool (world : World) (tool : OwnedTool) : Bool :=
  match (world.messages.filter fun message =>
      message.header.request == some tool.requestDoc &&
        message.header.session == tool.session && message.sequence == tool.acceptedSequence &&
        message.header.role == .assistant && message.header.outcome == .complete &&
        (match message.header.publication with | .requestExecution _ => true | _ => false)).dedup with
  | [message] =>
      (toolIntents message).any (fun intent => intent.call == tool.document) &&
        world.transcript.messages.any (fun row =>
          row.messageId == message.header.id && row.sessionId == tool.session &&
            row.sequence == tool.acceptedSequence && row.role == .assistant &&
            decide (row.kind.referencesToolCall tool.document))
  | _ => false

def metadataOwnedByGeneration (world : World) (generation : Generation)
    (tool : OwnedTool) : Bool :=
  let direct (document : DocId) :=
    tool.requestDoc == world.requestId && world.messages.any fun message =>
      message.header.request == some tool.requestDoc &&
        message.header.session == tool.session && message.sequence == tool.acceptedSequence &&
        message.header.role == .assistant && message.header.outcome == .complete &&
        message.header.publication == .requestExecution generation &&
        (toolIntents message).any (fun intent => intent.call == document)
  match tool.provenance with
  | .acceptedIntent => direct tool.document && directAcceptedHeaderMetadataBindsTool world tool
  | .spawnedBackground parentDoc =>
      direct parentDoc && tool.document != parentDoc &&
        match ownedToolByDocument? world parentDoc with
        | some parent => parent.provenance == .acceptedIntent &&
            parent.requestDoc == tool.requestDoc && parent.session == tool.session &&
            parent.acceptedSequence == tool.acceptedSequence &&
            directAcceptedHeaderMetadataBindsTool world parent
        | none => false

def spawnParentIntentValid (world : World) (tool : OwnedTool) (parentDoc : DocId) : Bool :=
  match ownedToolByDocument? world parentDoc with
  | some parent => parent.provenance == .acceptedIntent &&
      parent.requestDoc == tool.requestDoc && parent.session == tool.session &&
      parent.acceptedSequence == tool.acceptedSequence &&
      directAcceptedHeaderMetadataBindsTool world parent &&
      world.messages.any (fun message =>
        message.header.request == some tool.requestDoc &&
          message.header.session == tool.session &&
          message.sequence == tool.acceptedSequence &&
          (toolIntents message).any (fun intent =>
            intent.call == parentDoc && intent.name == "spawn_process"))
  | none => false

def acceptedHeaderBindsTool (world : World) (tool : OwnedTool) : Bool :=
  match tool.provenance with
  | .acceptedIntent => directAcceptedHeaderMetadataBindsTool world tool
  | .spawnedBackground parentDoc =>
      spawnParentIntentValid world tool parentDoc &&
        tool.document != parentDoc && tool.context.awaitMode == .background &&
        tool.context.childRequestId.isNone

def acceptedHeaderBindsToolGeneration (world : World) (tool : OwnedTool)
    (generation : Generation) : Bool :=
  let direct (document : DocId) :=
    tool.requestDoc == world.requestId && world.messages.any fun message =>
      message.header.request == some tool.requestDoc &&
        message.header.session == tool.session && message.sequence == tool.acceptedSequence &&
        message.header.role == .assistant && message.header.outcome == .complete &&
        message.header.publication == .requestExecution generation &&
        (toolIntents message).any (fun intent => intent.call == document)
  match tool.provenance with
  | .acceptedIntent => direct tool.document
  | .spawnedBackground parentDoc =>
      spawnParentIntentValid world tool parentDoc && direct parentDoc

def messageContainsToolResult (message : MessageEnvelope) (document : DocId) : Bool :=
  message.blocks.any fun block => match block with
  | .toolResult call _ _ _ => call == document
  | _ => false

def acceptedProviderId? (world : World) (tool : OwnedTool) : Option String :=
  match (world.messages.filter (fun message =>
      message.header.request == some tool.requestDoc &&
        message.header.session == tool.session &&
        message.sequence == tool.acceptedSequence &&
        message.header.role == .assistant && message.header.outcome == .complete &&
        (match message.header.publication with | .requestExecution _ => true | _ => false))).dedup with
  | [message] => (targetIntent message tool.document).map (fun intent => intent.providerId)
  | _ => none

def canonicalToolResultBound (world : World) (tool : OwnedTool)
    (key : Transcript.ToolResultKey) : Bool :=
  key.sessionId == tool.session && key.logicalResultId == tool.document &&
    (world.transcript.messages.filter (fun row =>
      row.kind == .toolResult tool.document key)).length == 1 &&
    match world.transcript.messages.filter (fun row =>
        row.kind == .toolResult tool.document key) with
    | [row] => match (world.messages.filter (fun message =>
        message.header.id == row.messageId && key.payloadHash == message.header.id &&
          message.header.session == row.sessionId &&
          message.header.request == some tool.requestDoc &&
          message.header.session == tool.session &&
          message.header.publication == .toolDelivery tool.document &&
          message.sequence == row.sequence && message.header.role == .user)).dedup with
      | [message] =>
          (match message.blocks with
          | [.toolResult call providerId _ _] =>
              call == tool.document && acceptedProviderId? world tool == some providerId
          | _ => false) &&
          match reconstructMessage world.segments noDeniedDocuments message with
          | .ok _ => true | .error _ => false
      | _ => false
    | _ => false

def runningReceiptSourceBound (world : World) (tool : OwnedTool) : Bool :=
  world.messages.any fun message =>
    message.header.publication == .toolDelivery tool.document &&
      message.header.request == some tool.requestDoc &&
      message.header.session == tool.session &&
      messageContainsToolResult message tool.document &&
      (envelopeRefs message).all (fun reference =>
        match resolveClose world.segments noDeniedDocuments reference with
        | .error _ => false
        | .ok closing =>
            closing.coordinate.request == tool.requestDoc &&
            (match closing.coordinate.source with | .authored _ => true | _ => false) &&
            closing.writer == .tool tool.document)

/-- Lifecycle-only projection used for terminal acknowledgements. It retains
exact physical/header ownership and transcript state but deliberately does not
reconstruct already-published result payloads, whose later corruption cannot
wedge a real host/tool acknowledgement. -/
def toolLifecycleProjectionCoherent (world : World) : Bool :=
  (world.toolContexts.map (fun tool => tool.document)).Nodup &&
    world.toolContexts.all (fun tool =>
      tool.session == world.sessionId && acceptedHeaderBindsTool world tool &&
        match tool.provenance with
        | .acceptedIntent =>
            match transcriptToolByDocument? world tool.document with
            | none => false
            | some row =>
                row.sessionId == tool.session &&
                  row.messageSequence == tool.acceptedSequence &&
                  row.state == tool.context.state &&
                  (decide (tool.document ∈ world.transcript.inFlight) ==
                    (tool.context.state == .running && !toolHandedOff tool))
        | .spawnedBackground _ =>
            (transcriptToolByDocument? world tool.document).isNone &&
              decide (tool.document ∉ world.transcript.inFlight)) &&
    world.transcript.toolCalls.all (fun row =>
      match ownedToolByDocument? world row.callId with
      | some tool => tool.provenance == .acceptedIntent && tool.session == row.sessionId &&
          tool.acceptedSequence == row.messageSequence
      | none => false)

/-- The physical document is the join key. Logical identifiers inside the
tool context never substitute for the accepted request/header binding. In
addition to lifecycle coherence, every published result key has exact canonical
authority; pending rows cannot carry results. A running result remains the
exact invocation receipt after an explicit foreground reattachment: current
await mode controls parent blocking, while `runningReceiptSourceBound` retains
the immutable receipt provenance. -/
def toolProjectionCoherent (world : World) : Bool :=
  toolLifecycleProjectionCoherent world && world.toolContexts.all (fun tool =>
    match tool.provenance, transcriptToolByDocument? world tool.document with
    | .acceptedIntent, some row =>
        match row.resultKey with
        | none => true
        | some key => canonicalToolResultBound world tool key &&
            (isTerminal row.state ||
              (row.state == .running && runningReceiptSourceBound world tool))
    | .spawnedBackground _, none => true
    | _, _ => false)

/-- A result key is delivery only when its unique transcript result row is
backed by the canonical user/tool-result message for this physical document. -/
def canonicalToolDelivered (world : World) (tool : OwnedTool) : Bool :=
  match transcriptToolByDocument? world tool.document with
  | none => false
  | some call => match call.resultKey with
    | none => false
    | some key => canonicalToolResultBound world tool key

def admissionsValid (world : World) (message : MessageEnvelope)
    (admissions : List ToolAdmission) : Bool :=
  admissions.map (fun admission => admission.document) ==
      (toolIntents message).map (fun intent => intent.call) &&
    (admissions.map (fun admission => admission.document)).Nodup &&
    admissions.all (fun admission =>
      admission.context.state == .pending &&
        !(world.toolContexts.any (fun tool => tool.document == admission.document)) &&
        (match world.remoteRoutes.filter (fun route => route.1 == admission.document) with
        | [] => admission.context.spawnBehaviorId.isNone
        | [route] => admission.context.awaitMode == .background &&
            admission.context.childRequestId.isSome &&
            admission.context.spawnBehaviorId == some route.2.2
        | _ => false
        ))

def installAcceptedTools (world : World) (message : MessageEnvelope)
    (admissions : List ToolAdmission) : List OwnedTool :=
  world.toolContexts ++ admissions.map (fun admission =>
    { document := admission.document
    , requestDoc := world.requestId
    , session := message.header.session
    , acceptedSequence := message.sequence
    , provenance := .acceptedIntent
    , context := admission.context
    , delegatedWorkspace := admission.delegatedWorkspace })

def spawnedAdmissionValid (world : World) (generation : Generation)
    (admission : SpawnedToolAdmission) : Bool :=
  !(world.toolContexts.any (fun tool => tool.document == admission.document)) &&
    !(world.toolContexts.any (fun tool =>
      tool.provenance == .spawnedBackground admission.parentToolDoc)) &&
    admission.document != admission.parentToolDoc &&
    admission.context.state == .pending &&
    admission.context.awaitMode == .background &&
    admission.context.childRequestId.isNone &&
    match ownedToolByDocument? world admission.parentToolDoc with
    | some parent => parent.provenance == .acceptedIntent &&
        parent.context.state == .running &&
        acceptedHeaderBindsToolGeneration world parent generation &&
        world.messages.any (fun message =>
          message.header.request == some parent.requestDoc &&
            message.header.session == parent.session &&
            message.sequence == parent.acceptedSequence &&
            (toolIntents message).any (fun intent =>
              intent.call == admission.parentToolDoc && intent.name == "spawn_process"))
    | none => false

def installSpawnedTool (world : World) (admission : SpawnedToolAdmission) :
    Option OwnedTool := do
  let parent ← ownedToolByDocument? world admission.parentToolDoc
  some
    { document := admission.document
    , requestDoc := parent.requestDoc
    , session := parent.session
    , acceptedSequence := parent.acceptedSequence
    , provenance := .spawnedBackground admission.parentToolDoc
    , context := admission.context }

def spawnedToolPresent (world : World) (admission : SpawnedToolAdmission) : Bool :=
  match ownedToolByDocument? world admission.document with
  | some tool => tool.provenance == .spawnedBackground admission.parentToolDoc &&
      ToolGenesis.fromContext tool.context == ToolGenesis.fromContext admission.context &&
      (transcriptToolByDocument? world admission.document).isNone
  | none => false

def spawnedAdmissionReplayValid (world : World)
    (admission : SpawnedToolAdmission) : Bool :=
  match world.toolContexts.filter (fun tool =>
      tool.provenance == .spawnedBackground admission.parentToolDoc) with
  | [tool] => tool.document == admission.document &&
      ToolGenesis.fromContext tool.context == ToolGenesis.fromContext admission.context &&
      acceptedHeaderBindsTool world tool
  | _ => false

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
          target.coordinator target.target target.behavior with
        | .error _ => .error .invalidDelegation
        | .ok row => .ok row
      let later ← prepareDelegatedCalls world segments message rest
      .ok (row :: later)

def remoteTargetsMatchConfiguredRoutes (world : World) (message : MessageEnvelope)
    (targets : List RemoteTarget) : Bool :=
  targets.map (fun target => (target.call, target.target, target.behavior)) == world.remoteRoutes &&
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

def eligibleOwnedAssistantMetadataExists (world : World) : Bool :=
  world.messages.any fun message =>
    message.header.request == some world.requestId &&
      message.header.session == world.sessionId && message.header.role == .assistant &&
      terminalPublicationEligible message.header.publication

/-- Exceptional policy revocation resolves only immutable header identity.
Conflicting payload bytes remain visible and are neither selected nor repaired. -/
def terminalSelectionMetadataValid (world : World)
    (selection : TerminalSelection) : Bool :=
  match selection with
  | .noMessage => !eligibleOwnedAssistantMetadataExists world
  | .message _ => match resolveTerminal (world.messages.map (fun message => message.header))
      noDeniedDocuments world.requestId world.sessionId (some selection) with
    | .error _ => false
    | .ok none => false
    | .ok (some header) =>
        (world.messages.filter (fun message => message.header == header)).dedup.length == 1

end CanonicalOutput.Execution
