import Proofs.CanonicalOutput.Execution.Projection

namespace CanonicalOutput.Execution

open RequestExecutionLease

def synchronize (world : World) : Except Error World :=
  match authoritativeLease world with
  | .error error => .error (.integrity error)
  | .ok lease => .ok { world with lease := lease }

def synchronizePost (world : World) : Except Error World := synchronize world

def activeGeneration? (world : World) : Option Generation :=
  match world.lease.lease with
  | .active generation _ _ => some generation
  | _ => none

def requestSegmentShape (world : World) (generation : Generation)
    (record : Segment) : Bool :=
  record.coordinate.request == world.requestId &&
    record.writer == .request generation &&
    writerMatchesSource record.coordinate record.writer &&
    record.createdAt == world.lease.now

def providerSource : Source → Bool
  | .provider _ _ _ => true
  | _ => false

def freshSegmentIdentity (world : World) (record : Segment) : Bool :=
  !(world.segments.any fun old => old.id == record.id)

def freshMessageIdentity (world : World) (message : MessageEnvelope) : Bool :=
  !(world.messages.any fun old =>
    old.header.id == message.header.id ||
      (old.header.session == message.header.session && old.key == message.key) ||
      (old.header.session == message.header.session && old.sequence == message.sequence))

def checked (predicate : World → Bool) : Except Error World → Except Error World
  | .error error => .error error
  | .ok world => if predicate world then .ok world else .error .publicationIncomplete

def rawReplayShape (world : World) (generation : Generation) (record : Segment) : Bool :=
  record.coordinate.request == world.requestId &&
    record.writer == .request generation && providerSource record.coordinate.source &&
    record.flush.isSome && record.close.isNone

/-- Guarded raw output insert. Exact replay is identity and can be acknowledged
after expiry; a distinct record with the same identity is never replay. -/
def appendRawCore (world : World) (generation : Generation)
    (record : Segment) : Except Error World :=
  if segmentIdentityCollision world record then .error .identityCollision
  else if record ∈ world.segments then
    if rawReplayShape world generation record then synchronize world else .error .invalidSegment
  else if requestSegmentShape world generation record = false ∨
      providerSource record.coordinate.source = false ∨
      record.flush.isNone ∨ record.close.isSome ∨
      sourceOpen world record.coordinate = false then .error .invalidSegment
  else match synchronize world with
  | .error error => .error error
  | .ok authoritative =>
      match RequestExecutionLease.step? authoritative.lease
          (.appendOutput .mutationWriteGate generation record.id .currentRequest) with
      | none => .error .leaseRejected
      | some lease =>
          synchronizePost
            { authoritative with
              lease := lease
              segments := authoritative.segments ++ [record] }

def retractionShape (world : World) (generation : Generation)
    (record : Segment) : Bool :=
  requestSegmentShape world generation record && record.flush.isNone &&
    providerSource record.coordinate.source && record.close == some .retracted

def retractionReplayShape (world : World) (generation : Generation)
    (record : Segment) : Bool :=
  record.coordinate.request == world.requestId &&
    record.writer == .request generation && providerSource record.coordinate.source &&
    record.flush.isNone && record.close == some .retracted

/-- The retry boundary appends an immutable Retracted closure before control may
return to retry policy. It does not itself choose a new provider attempt. -/
def retractBeforeRetryCore (world : World) (generation : Generation)
    (record : Segment) : Except Error World :=
  if segmentIdentityCollision world record then .error .identityCollision
  else if record ∈ world.segments then
    if retractionReplayShape world generation record then synchronize world
    else .error .invalidSegment
  else if retractionShape world generation record = false then .error .invalidSegment
  else if sourceOpen world record.coordinate = false then .error .sourceAlreadyClosed
  else match synchronize world with
  | .error error => .error error
  | .ok authoritative =>
      match RequestExecutionLease.step? authoritative.lease
          (.authorizeProducerDecision .mutationWriteGate generation .closeOrRetract) with
      | none => .error .leaseRejected
      | some lease =>
          synchronizePost
            { authoritative with
              lease := lease
              segments := authoritative.segments ++ [record] }

def closedComplete (record : Segment) : Bool :=
  match record.close with
  | some (.closed .complete _ _) => true
  | _ => false

def closedPartial (record : Segment) : Bool :=
  match record.close with
  | some (.closed .«partial» _ _) => true
  | _ => false

def validateClosingRecord (segments : List Segment) (closing : Segment) : Bool :=
  match uniqueRecord LookupError.unavailable .conflictingClosures
      (closures segments closing.coordinate) with
  | .ok only => only == closing &&
      match reconstructExtent segments closing with
      | .ok _ => true
      | .error _ => false
  | .error _ => false

/-- Fresh publication closes the entire authoritative data extent visible at
its gate. Reconstruction remains deliberately tolerant of later facts beyond a
previously committed extent. -/
def freshCompleteExtentExact (segments : List Segment) (closing : Segment) : Bool :=
  match closing.close with
  | some (.closed .complete count _) =>
      let data := sourceData segments closing.coordinate
      count == data.length &&
        (match validateOpenPrefix segments closing.coordinate closing.writer with
        | .ok _ => true
        | .error _ => false) &&
        timestampsNondecreasing data &&
        data.all (fun record => record.createdAt ≤ closing.createdAt)
  | _ => false

def transcriptPublicationRowPresent (transcript : Transcript.TranscriptState) (header : Header)
    (turn : Transcript.AssistantTurn) : Bool :=
  transcript.messages.any fun row =>
    row.messageId == header.id && row.sessionId == turn.sessionId &&
      row.sequence == turn.sequence && row.role == .assistant &&
      turn.callIds.all fun callId => row.kind.referencesToolCall callId

def publicationRowPresent (world : World) (header : Header)
    (turn : Transcript.AssistantTurn) : Bool :=
  transcriptPublicationRowPresent world.transcript header turn

def toolIntentPresent (world : World) (turn : Transcript.AssistantTurn)
    (callId : ToolExecution.ToolCallId) : Bool :=
  world.transcript.toolCalls.any fun call =>
    call.callId == callId && call.sessionId == turn.sessionId &&
      call.messageSequence == turn.sequence

def delegatedRowsPresent (world : World) (rows : List DelegatedCall) : Bool :=
  rows.all fun row => row ∈ world.delegatedCalls

def acceptedPublicationPresent (world : World) (closing : Segment)
    (message : MessageEnvelope) (targets : List RemoteTarget) : Bool :=
  let turn := messageTurn message
  closing ∈ world.segments && message ∈ world.messages &&
    publicationRowPresent world message.header turn &&
    turn.callIds.all (toolIntentPresent world turn) &&
    match prepareDelegatedCalls world world.segments message targets with
    | .error _ => false
    | .ok rows => delegatedRowsPresent world rows

/-- Accepted provider turn transaction: validated Complete closure, typed native
message, assistant transcript row, every ordered pending tool intent, and each
requested remote-only delegated argument row appear together before dispatch. -/
def acceptAndPublishCore (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) (targets : List RemoteTarget) :
    Except Error World :=
  let header := message.header
  let turn := messageTurn message
  if segmentIdentityCollision world closing ∨ messageIdentityCollision world message then
    .error .identityCollision
  else if ¬ (targets.map (fun target => target.call)).Nodup then .error .invalidDelegation
  else if remoteTargetsMatchConfiguredRoutes world message targets = false then
    .error .invalidDelegation
  else if acceptedPublicationPresent world closing message targets then
    if closedComplete closing && validateClosingRecord world.segments closing &&
        acceptedMessageValid world generation world.segments message &&
        acceptedSourceBound world generation closing message then synchronize world
    else .error .publicationIncomplete
  else if freshSegmentIdentity world closing = false ∨
      freshMessageIdentity world message = false then .error .identityCollision
  else if requestSegmentShape world generation closing = false ∨
      closedComplete closing = false then .error .invalidSegment
  else if message.createdAt != world.lease.now ||
      acceptedSourceBound world generation closing message = false then .error .invalidHeader
  else if sourceOpen world closing.coordinate = false then .error .sourceAlreadyClosed
  else if world.transcript.sessionId ≠ world.sessionId ∨
      turn.sessionId ≠ world.sessionId ∨ turn.outcome ≠ .complete then
    .error .transcriptRejected
  else
    let segments := world.segments ++ [closing]
    if validateClosingRecord segments closing = false ∨
        freshCompleteExtentExact segments closing = false ∨
        acceptedMessageValid world generation segments message = false ∨
        acceptedSourceBound world generation closing message = false then
      .error .invalidHeader
    else if ¬ world.transcript.PublishableTurn turn then .error .transcriptRejected
    else match prepareDelegatedCalls world segments message targets with
    | .error error => .error error
    | .ok delegated => match synchronize world with
      | .error error => .error error
      | .ok authoritative =>
          match RequestExecutionLease.step? authoritative.lease
              (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish) with
          | none => .error .leaseRejected
          | some lease =>
              synchronizePost
                { authoritative with
                  lease := lease
                  segments := segments
                  messages := authoritative.messages ++ [message]
                  transcript := authoritative.transcript.publishAcceptedAssistant header.id turn
                  delegatedCalls := authoritative.delegatedCalls ++ delegated }

def authoredRowPresent (world : World) (message : MessageEnvelope) : Bool :=
  match message.header.role with
  | .system => message.header.session == world.transcript.sessionId &&
      message.sequence < world.transcript.nextSeq
  | .user => world.transcript.messages.any fun row =>
      row.messageId == message.header.id && row.sessionId == message.header.session &&
        row.sequence == message.sequence && row.role == .user && row.kind == .ordinary
  | .assistant => false

def appendAuthoredRow (transcript : Transcript.TranscriptState)
    (message : MessageEnvelope) : Transcript.TranscriptState :=
  match message.header.role with
  | .user => transcript.appendUserMessage message.header.id .ordinary
  | .system => { transcript with nextSeq := transcript.nextSeq + 1 }
  | .assistant => transcript

def authoredPublicationPresent (world : World) (closing : Segment)
    (message : MessageEnvelope) : Bool :=
  closing ∈ world.segments && message ∈ world.messages && authoredRowPresent world message

/-- A whole authored user/system message is one transaction: dense immutable
payload, Complete closure, and typed header. Authored sources cannot enter via
`appendRaw`, so there is no durable unheaded authored intermediate state. -/
def publishAuthoredCore (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) : Except Error World :=
  if segmentIdentityCollision world closing || messageIdentityCollision world message then
    .error .identityCollision
  else if authoredPublicationPresent world closing message then
    if closedComplete closing && validateClosingRecord world.segments closing &&
        authoredMessageValid world generation world.segments closing message then synchronize world
    else .error .publicationIncomplete
  else if freshSegmentIdentity world closing = false ||
      freshMessageIdentity world message = false then .error .identityCollision
  else if requestSegmentShape world generation closing = false ||
      closedComplete closing = false || message.createdAt != world.lease.now then
    .error .invalidSegment
  else if sourceOpen world closing.coordinate = false then .error .sourceAlreadyClosed
  else if world.transcript.sessionId != world.sessionId ||
      message.sequence != world.transcript.nextSeq then .error .transcriptRejected
  else
    let segments := world.segments ++ [closing]
    if validateClosingRecord segments closing = false ||
        freshCompleteExtentExact segments closing = false ||
        authoredMessageValid world generation segments closing message = false then
      .error .invalidHeader
    else match synchronize world with
    | .error error => .error error
    | .ok authoritative =>
        match RequestExecutionLease.step? authoritative.lease
            (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish) with
        | none => .error .leaseRejected
        | some lease => synchronizePost
            { authoritative with
              lease := lease
              segments := segments
              messages := authoritative.messages ++ [message]
              transcript := appendAuthoredRow authoritative.transcript message }

def headerOnlyPublicationPresent (world : World) (message : MessageEnvelope) : Bool :=
  message ∈ world.messages && match message.header.role with
  | .assistant => publicationRowPresent world message.header (messageTurn message)
  | .user => authoredRowPresent world message
  | .system => false

def appendHeaderOnlyRow (transcript : Transcript.TranscriptState)
    (message : MessageEnvelope) : Transcript.TranscriptState :=
  match message.header.role with
  | .assistant => transcript.publishAcceptedAssistant message.header.id (messageTurn message)
  | .user => transcript.appendUserMessage message.header.id .ordinary
  | .system => transcript

/-- Payload-free Complete assistant/user publication needs no synthetic source.
Its header and transcript row still commit atomically behind the request gate. -/
def publishHeaderOnlyCore (world : World) (generation : Generation)
    (message : MessageEnvelope) : Except Error World :=
  if messageIdentityCollision world message then .error .identityCollision
  else if headerOnlyPublicationPresent world message then
    if headerOnlyMessageValid world generation message then synchronize world
    else .error .publicationIncomplete
  else if freshMessageIdentity world message = false then .error .identityCollision
  else if message.createdAt != world.lease.now ||
      headerOnlyMessageValid world generation message = false then .error .invalidHeader
  else if world.transcript.sessionId != world.sessionId ||
      message.sequence != world.transcript.nextSeq then .error .transcriptRejected
  else if message.header.role == .assistant &&
      ¬ world.transcript.PublishableTurn (messageTurn message) then .error .transcriptRejected
  else match synchronize world with
  | .error error => .error error
  | .ok authoritative =>
      match RequestExecutionLease.step? authoritative.lease
          (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish) with
      | none => .error .leaseRejected
      | some lease => synchronizePost
          { authoritative with
            lease := lease
            messages := authoritative.messages ++ [message]
            transcript := appendHeaderOnlyRow authoritative.transcript message }

def dispatchPublicationValid (world : World) (generation : Generation)
    (callId : ToolExecution.ToolCallId) : Bool :=
  let candidates := (world.messages.filter fun message =>
    message.header.session == world.sessionId && message.header.role == .assistant &&
      (toolIntents message).any (fun intent => intent.call == callId)).dedup
  match candidates with
  | [message] =>
      acceptedMessageValid world generation world.segments message &&
        publicationRowPresent world message.header (messageTurn message) &&
        toolIntentPresent world (messageTurn message) callId &&
        match targetIntent message callId with
        | none => false
        | some intent => match resolveClose world.segments noDeniedDocuments intent.arguments with
          | .error _ => false
          | .ok closing => closedComplete closing &&
              validateClosingRecord world.segments closing &&
              acceptedSourceBound world generation closing message &&
              match world.remoteRoutes.filter (fun route => route.1 == callId) with
              | [] => true
              | [route] => match prepareDelegatedCalls world world.segments message
                  [⟨callId, world.principal, route.2⟩] with
                | .error _ => false
                | .ok rows => delegatedRowsPresent world rows
              | _ => false
  | _ => false

def dispatchCore (world : World) (generation : Generation)
    (permit : DispatchPermit) : Except Error World :=
  let callId := permit.call
  if dispatchPublicationValid world generation callId = false then .error .publicationIncomplete
  else if world.transcript.RunningPublishedCall callId then synchronize world
  else if !permit.cancellationAllows || !permit.toolPolicyAllows then .error .transcriptRejected
  else if ¬ world.transcript.ReadyToDispatch callId then .error .transcriptRejected
  else match synchronize world with
  | .error error => .error error
  | .ok authoritative =>
      match RequestExecutionLease.step? authoritative.lease
          (.authorizeProducerDecision .mutationWriteGate generation .dispatch) with
      | none => .error .leaseRejected
      | some lease => .ok
          { authoritative with
            lease := lease
            transcript := authoritative.transcript.dispatchToolCall callId }

def recoveryExtentExact (world : World) (expected : Generation)
    (closing : Segment) : Bool :=
  match closing.close with
  | some (.closed .«partial» count _) =>
      count == (sourceData world.segments closing.coordinate).length &&
        match validateOpenPrefix world.segments closing.coordinate (.request expected) with
        | .ok _ => true
        | .error _ => false
  | _ => false

def providerCoordinate : Coordinate → Bool
  | ⟨_, .provider _ _ _⟩ => true
  | _ => false

def partialClosureBy (generation : Generation) (record : Segment) : Bool :=
  writtenBy generation record && closedPartial record

/-- Provider sources that would be stranded by a generation swap: they have an
old-generation fact and are either still open or already carry the one reusable
Partial closure from an acknowledged-lost recovery attempt. Complete and
Retracted sources are already decided. -/
def recoverableCoordinates (world : World) (expected : Generation) : List Coordinate :=
  (requestCoordinates world).filter fun coordinate =>
    providerCoordinate coordinate &&
      (sourceRecords world.segments coordinate).any (writtenBy expected) &&
      match (closures world.segments coordinate).dedup with
      | [] => true
      | [closing] => partialClosureBy expected closing &&
          !(world.messages.any fun message =>
            message.header.refs.any (fun ref => ref.closeId == closing.id))
      | _ => false

def recoveryCoordinates (items : List RecoveryItem) : List Coordinate :=
  items.map (fun item => item.closing.coordinate)

def recoveryCoversAllSources (world : World) (expected : Generation)
    (items : List RecoveryItem) : Bool :=
  let supplied := recoveryCoordinates items
  supplied.Nodup &&
    (recoverableCoordinates world expected).all (fun coordinate => coordinate ∈ supplied) &&
    supplied.all (fun coordinate => coordinate ∈ recoverableCoordinates world expected)

def reusableOrFreshClosure (world : World) (expected : Generation)
    (closing : Segment) : Bool :=
  match (closures world.segments closing.coordinate).dedup with
  | [] => freshSegmentIdentity world closing &&
      requestSegmentShape world expected closing && closing.flush.isNone &&
      closedPartial closing && recoveryExtentExact world expected closing
  | [existing] => existing == closing && partialClosureBy expected existing &&
      recoveryExtentExact world expected closing
  | _ => false

def installRecoveryClosures (segments : List Segment)
    (items : List RecoveryItem) : List Segment :=
  items.foldl (fun result item =>
    if item.closing ∈ result then result else result ++ [item.closing]) segments

def prepareRecoveryItems (world : World) (expected fresh : Generation) :
    List RecoveryItem → RecoveryPrepared → Except Error RecoveryPrepared
  | [], prepared => .ok prepared
  | item :: rest, prepared =>
      let current : World :=
        { world with
          segments := prepared.segments
          messages := prepared.messages
          transcript := prepared.transcript }
      if segmentIdentityCollision current item.closing then .error .identityCollision
      else if reusableOrFreshClosure world expected item.closing = false then
        .error .invalidSegment
      else
        let segments := if item.closing ∈ prepared.segments then prepared.segments
          else prepared.segments ++ [item.closing]
        match reconstructExtent segments item.closing with
        | .error _ => .error .invalidSegment
        | .ok streams =>
            if recoveryMessageValid world fresh segments item.closing streams item.message = false
              then .error .invalidRecoveryHeader
            else match item.message with
            | none => prepareRecoveryItems world expected fresh rest
                { prepared with segments := segments }
            | some message =>
                let withSegments : World := { current with segments := segments }
                if message ∈ prepared.messages then
                  if publicationRowPresent withSegments message.header (messageTurn message) then
                    prepareRecoveryItems world expected fresh rest
                      { prepared with segments := segments }
                  else .error .publicationIncomplete
                else if freshMessageIdentity withSegments message = false ||
                    message.createdAt != world.lease.now then .error .identityCollision
                else if ¬ prepared.transcript.PublishableTurn (messageTurn message) then
                  .error .transcriptRejected
                else prepareRecoveryItems world expected fresh rest
                  { segments := segments
                    messages := prepared.messages ++ [message]
                    transcript := prepared.transcript.publishPartialAssistant
                      message.header.id (messageTurn message) }

def prepareRecoveryBatch (world : World) (expected fresh : Generation)
    (items : List RecoveryItem) : Except Error RecoveryPrepared :=
  if recoveryCoversAllSources world expected items = false then .error .invalidSegment
  else prepareRecoveryItems world expected fresh items
    ⟨world.segments, world.messages, world.transcript⟩

def recoveryBatchValid (world : World) (expected fresh : Generation)
    (items : List RecoveryItem) : Bool :=
  match prepareRecoveryBatch world expected fresh items with
  | .ok _ => true
  | .error _ => false

def recoveryItemPresent (world : World) (item : RecoveryItem) : Bool :=
  item.closing ∈ world.segments && match item.message with
  | none => true
  | some message => message ∈ world.messages &&
      publicationRowPresent world message.header (messageTurn message)

def recoveryBatchPresent (world : World) (fresh : Generation)
    (items : List RecoveryItem) : Bool :=
  world.currentGeneration? == some fresh && items.all (recoveryItemPresent world)

/-- Exact lost-acknowledgement validation is independent of whether these
coordinates remain eligible for a new recovery. It binds the old writer and
fresh recovery header, but does not compare a replayed header timestamp with
the current clock or reject later facts outside the committed closure extent. -/
def recoveryReplayValid (world : World) (expected fresh : Generation)
    (items : List RecoveryItem) : Bool :=
  (recoveryCoordinates items).Nodup && world.currentGeneration? == some fresh &&
    items.all fun item =>
      item.closing.coordinate.request == world.requestId &&
        providerCoordinate item.closing.coordinate &&
        (closures world.segments item.closing.coordinate).dedup == [item.closing] &&
        item.closing ∈ world.segments && partialClosureBy expected item.closing &&
        match reconstructExtent world.segments item.closing with
        | .error _ => false
        | .ok streams =>
            recoveryMessageValid world fresh world.segments item.closing streams item.message &&
              match item.message with
              | none => true
              | some message => message ∈ world.messages &&
                  publicationRowPresent world message.header (messageTurn message)

/-- One gate transaction closes every unresolved keyed provider source before
the old generation becomes inaccessible, reusing exact Partial closures when
already present. Optional conservative recovery headers/rows and the fresh
generation swap commit atomically. -/
def recoverExpiredBatchCore (world : World) (expected fresh : Generation)
    (duration deadline : Time) (items : List RecoveryItem) : Except Error World :=
  if recoveryReplayValid world expected fresh items then
    synchronize world
  else match prepareRecoveryBatch world expected fresh items with
  | .error error => .error error
  | .ok prepared => match synchronize world with
    | .error error => .error error
    | .ok authoritative =>
        match RequestExecutionLease.step? authoritative.lease
            (.recoverExpired .mutationWriteGate expected fresh duration deadline) with
        | none => .error .leaseRejected
        | some lease => synchronizePost
            { authoritative with
              lease := lease
              segments := prepared.segments
              messages := prepared.messages
              transcript := prepared.transcript }

def terminalReplayPresent (world : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) : Bool :=
  world.lease.lease == .terminal generation outcome &&
    world.terminalSelection == some selection

def acceptedOwnedCalls (world : World) :
    List (SessionId × Transcript.Sequence × ToolExecution.ToolCallId) :=
  world.messages.flatMap fun message =>
    match message.header.publication with
    | .requestExecution generation =>
        if acceptedMessageValid world generation world.segments message &&
            publicationRowPresent world message.header (messageTurn message) then
          (toolIntents message).map (fun intent =>
            (message.header.session, message.sequence, intent.call))
        else []
    | _ => []

def terminalizeOwnedPending (world : World) : Transcript.TranscriptState :=
  world.transcript.cancelPendingOwnedCalls (acceptedOwnedCalls world)

def ownedPendingSettled (world : World) : Bool :=
  world.transcript.toolCalls.all fun row =>
    if (row.sessionId, row.messageSequence, row.callId) ∈ acceptedOwnedCalls world then
      row.state != .pending
    else true

/-- Final request lifecycle and exact terminal-output selection commit together.
No latest-message fallback is available. -/
def terminalizeCore (world : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) :
    Except Error World :=
  if terminalSelectionValid world selection = false then .error .terminalRejected
  else if terminalReplayPresent world generation outcome selection then synchronize world
  else if world.terminalSelection.isSome then .error .terminalRejected
  else match synchronize world with
  | .error error => .error error
  | .ok authoritative =>
      match RequestExecutionLease.step? authoritative.lease
          (.finalize .mutationWriteGate generation outcome) with
      | none => .error .leaseRejected
      | some lease => .ok
          { authoritative with
            lease := lease
            transcript := terminalizeOwnedPending authoritative
            terminalSelection := some selection }

def appendRaw (world : World) (generation : Generation)
    (record : Segment) : Except Error World :=
  checked (fun post =>
    record ∈ post.segments && post.lease.request == world.lease.request &&
      post.lease.lease == world.lease.lease)
    (appendRawCore world generation record)

def retractBeforeRetry (world : World) (generation : Generation)
    (record : Segment) : Except Error World :=
  checked (fun post =>
    record ∈ post.segments &&
      (closures post.segments record.coordinate).dedup == [record])
    (retractBeforeRetryCore world generation record)

def acceptAndPublish (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) (targets : List RemoteTarget) :
    Except Error World :=
  checked (fun post =>
    acceptedPublicationPresent post closing message targets &&
      remoteTargetsMatchConfiguredRoutes post message targets &&
      validateClosingRecord post.segments closing &&
      acceptedMessageValid post generation post.segments message &&
      acceptedSourceBound post generation closing message)
    (acceptAndPublishCore world generation closing message targets)

def dispatch (world : World) (generation : Generation)
    (permit : DispatchPermit) : Except Error World :=
  checked (fun post => decide (post.transcript.RunningPublishedCall permit.call) &&
    dispatchPublicationValid post generation permit.call)
    (dispatchCore world generation permit)

def publishAuthored (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) : Except Error World :=
  checked (fun post =>
    authoredPublicationPresent post closing message &&
      validateClosingRecord post.segments closing &&
      authoredMessageValid post generation post.segments closing message)
    (publishAuthoredCore world generation closing message)

def publishHeaderOnly (world : World) (generation : Generation)
    (message : MessageEnvelope) : Except Error World :=
  checked (fun post =>
    headerOnlyPublicationPresent post message &&
      headerOnlyMessageValid post generation message)
    (publishHeaderOnlyCore world generation message)

def recoverExpiredBatch (world : World) (expected fresh : Generation)
    (duration deadline : Time) (items : List RecoveryItem) :
    Except Error World :=
  checked (fun post =>
    recoveryBatchPresent post fresh items &&
      (recoveryReplayValid world expected fresh items ||
        recoveryCoversAllSources world expected items) &&
      items.all (fun item =>
        (closures post.segments item.closing.coordinate).dedup == [item.closing] &&
          item.closing.writer == .request expected &&
          (recoveryReplayValid world expected fresh items ||
            recoveryExtentExact world expected item.closing)))
    (recoverExpiredBatchCore world expected fresh duration deadline items)

def terminalize (world : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) :
    Except Error World :=
  checked (fun post =>
    terminalReplayPresent post generation outcome selection &&
      terminalSelectionValid post selection && ownedPendingSettled post)
    (terminalizeCore world generation outcome selection)

end CanonicalOutput.Execution
