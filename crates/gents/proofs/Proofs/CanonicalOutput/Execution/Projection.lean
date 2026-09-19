import Proofs.CanonicalOutput.Execution.State

namespace CanonicalOutput.Execution

open RequestExecutionLease

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

/-- Identity collisions matter to this lease only when one side claims the
current request generation. Foreign/tool/old-generation identities do not become
request-liveness evidence through this function. -/
def currentIdentitiesValid (world : World) (generation : Generation) : Bool :=
  (requestRecords world).all fun record =>
    if writtenBy generation record then exactIdentityAt world.segments record else true

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
  if data.all (fun record => record.writer == writer) = false then
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

def factsFromSegments (generation : Generation) (records : List Segment) :
    List (OutputFact Generation) :=
  records.filterMap fun record =>
    if writtenBy generation record ∧ (record.flush.isSome ∨ record.close.isSome) then
      some ⟨record.id, generation, record.createdAt, .currentRequest⟩
    else none

def headerGeneration? (world : World) (message : MessageEnvelope) : Option Generation :=
  if message.header.request != some world.requestId ||
      message.header.session != world.sessionId then none
  else match message.header.publication with
    | .requestExecution generation | .requestRecovery generation => some generation
    | .toolDelivery _ | .fork _ => none

def headerCoordinateConflict (world : World) (message : MessageEnvelope) : Bool :=
  world.messages.any fun other => other != message &&
    (other.header.id == message.header.id ||
      (other.header.session == message.header.session && other.key == message.key) ||
      (other.header.session == message.header.session && other.sequence == message.sequence))

def currentHeadersValid (world : World) (generation : Generation) : Bool :=
  world.messages.all fun message =>
    if headerGeneration? world message == some generation then
      !headerCoordinateConflict world message &&
        match reconstructMessage world.segments noDeniedDocuments message with
        | .ok _ => true
        | .error _ => false
    else true

def factsFromHeaders (world : World) (generation : Generation) :
    List (OutputFact Generation) :=
  world.messages.filterMap fun message =>
    if headerGeneration? world message == some generation then
      some ⟨message.header.id, generation, message.createdAt, .currentRequest⟩
    else none

/-- Progress for one keyed source. A visible closure fixes the selected extent
before ordinal-twin validation, so late records beyond recovery's extent are
inert. A retracted or superseded-generation source contributes no liveness. -/
def sourceProgress (world : World) (generation : Generation)
    (coordinate : Coordinate) : Except IntegrityError (List (OutputFact Generation)) := do
  let records := sourceRecords world.segments coordinate
  let closeRecords := (records.filter (fun record => record.close.isSome)).dedup
  if !records.any (writtenBy generation) then .ok []
  else match closeRecords with
  | [] =>
      let writer := Writer.request generation
      if !writerMatchesSource coordinate writer then .error (.invalidWriter coordinate)
      else match validateOpenPrefix world.segments coordinate writer with
        | .ok _ => .ok (factsFromSegments generation (sourceData world.segments coordinate))
        | .error error => .error error
  | [closing] =>
      match closing.close with
      | some .retracted =>
          if writtenBy generation closing then .ok (factsFromSegments generation [closing])
          else .ok []
      | some (.closed _ count _) =>
          if !writtenBy generation closing then .error (.invalidWriter coordinate)
          else match reconstructExtent world.segments closing with
          | .error _ => .error (.malformedSource coordinate)
          | .ok _ =>
              let selected := (extent world.segments coordinate count ++ [closing]).dedup
              if timestampsNondecreasing (extent world.segments coordinate count) &&
                  (extent world.segments coordinate count).all
                    (fun record => record.createdAt ≤ closing.createdAt) then
                .ok (factsFromSegments generation selected)
              else .error (.malformedSource coordinate)
      | none => .error (.malformedSource coordinate)
  | _ => .error (.closureConflict coordinate)

def collectProgress (world : World) (generation : Generation) :
    List Coordinate → Except IntegrityError (List (OutputFact Generation))
  | [] => .ok []
  | coordinate :: rest => do
      let here ← sourceProgress world generation coordinate
      let later ← collectProgress world generation rest
      .ok (here ++ later)

def factBackedByCanonicalRecord (world : World) (generation : Generation)
    (fact : OutputFact Generation) : Bool :=
  (world.segments.any fun record =>
    record.id == fact.id && record.coordinate.request == world.requestId &&
      record.writer == .request generation && record.createdAt == fact.createdAt &&
      (record.flush.isSome || record.close.isSome)) ||
  (world.messages.any fun message =>
    message.header.id == fact.id && message.createdAt == fact.createdAt &&
      headerGeneration? world message == some generation)

/-- The only constructor of lease progress in the composed owner. Eligibility is
derived from actual scoped canonical records rather than accepted as a durable tag. -/
def deriveProgress (world : World) (generation : Generation) :
    Except IntegrityError (List (OutputFact Generation)) :=
  if currentIdentitiesValid world generation = false then .error .identityConflict
  else if currentHeadersValid world generation = false then .error .headerConflict
  else match collectProgress world generation (requestCoordinates world) with
  | .error error => .error error
  | .ok facts =>
      let projected := (facts ++ factsFromHeaders world generation).dedup |>.map fun fact =>
        { fact with generation := generation, eligibility := .currentRequest }
      if projected.all (factBackedByCanonicalRecord world generation) then .ok projected
      else .error .identityConflict

def authoritativeLease (world : World) : Except IntegrityError
    (RequestExecutionLease.World Generation) :=
  match world.lease.lease with
  | .active generation _ _ => do
      let output ← deriveProgress world generation
      .ok { world.lease with output := output }
  | .recoverable generation _ _ => do
      let output ← deriveProgress world generation
      .ok { world.lease with output := output }
  | _ => .ok { world.lease with output := [] }

def withAuthoritativeLease (world : World)
    (lease : RequestExecutionLease.World Generation) : World :=
  { world with lease := lease }

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
