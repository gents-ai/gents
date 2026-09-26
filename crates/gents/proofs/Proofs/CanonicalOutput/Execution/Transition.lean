import Proofs.CanonicalOutput.Execution.Projection

namespace CanonicalOutput.Execution

open RequestExecutionLease

/-- Explicit owner heartbeat. Due time, generation fencing, stale-deadline
rejection, and the new deadline are all decided by the lease state machine.
Unlike output operations, renewal does not inspect or reconstruct canonical
output. -/
def renewCore (world : World) (generation : Generation)
    (expectedDeadline : Time) : Except Error World :=
  match RequestExecutionLease.step? world.lease
      (.renew .mutationWriteGate generation expectedDeadline) with
  | none => .error .leaseRejected
  | some lease => .ok { world with lease := lease }

def renew (world : World) (generation : Generation)
    (expectedDeadline : Time) : Except Error World :=
  renewCore world generation expectedDeadline

def requestSegmentShape (world : World) (generation : Generation)
    (record : Segment) : Bool :=
  record.coordinate.request == world.requestId &&
    record.writer == .request generation &&
    writerMatchesSource record.coordinate record.writer &&
    record.createdAt == world.lease.now

def providerSource : Source → Bool
  | .provider _ _ _ => true
  | .auxiliary _ _ _ _ => true
  | _ => false

def freshSegmentIdentity (world : World) (record : Segment) : Bool :=
  !(world.segments.any fun old => old.id == record.id)

def freshMessageIdentity (world : World) (message : MessageEnvelope) : Bool :=
  !(world.messages.any fun old =>
    old.header.id == message.header.id ||
      (old.header.session == message.header.session && old.key == message.key) ||
      (old.header.session == message.header.session && old.sequence == message.sequence))

theorem mapError_success {error₁ error₂ value : Type}
    (f : error₁ → error₂) (result : Except error₁ value) (post : value)
    (h : result.mapError f = .ok post) : result = .ok post := by
  cases result with
  | error error => simp [Except.mapError] at h
  | ok value => simpa [Except.mapError] using h

def checked (predicate : World → Bool) : Except Error World → Except Error World
  | .error error => .error error
  | .ok world => if predicate world then .ok world else .error .publicationIncomplete

theorem checked_core_success (predicate : World → Bool) (result : Except Error World)
    (after : World) (h : checked predicate result = .ok after) : result = .ok after := by
  cases he : result with
  | error error => simp [checked, he] at h
  | ok post =>
      simp only [checked, he] at h
      split at h
      · cases h; rfl
      · contradiction

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
    if rawReplayShape world generation record then .ok world else .error .invalidSegment
  else if requestSegmentShape world generation record = false ∨
      providerSource record.coordinate.source = false ∨
      record.flush.isNone ∨ record.close.isSome ∨
      sourceOpen world record.coordinate = false then .error .invalidSegment
  else match RequestExecutionLease.step? world.lease
      (.appendOutput .mutationWriteGate generation) with
  | none => .error .leaseRejected
  | some lease =>
      let candidate :=
        { world with lease := lease, segments := world.segments ++ [record] }
      match validateOpenPrefix candidate.segments record.coordinate record.writer with
      | .error error => .error (.integrity error)
      | .ok _ => .ok candidate

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
  else if sourceIdentitiesValid world.segments record.coordinate = false then
    .error (.integrity .identityConflict)
  else if record ∈ world.segments then
    if retractionReplayShape world generation record then .ok world
    else .error .invalidSegment
  else if retractionShape world generation record = false then .error .invalidSegment
  else if sourceOpen world record.coordinate = false then .error .sourceAlreadyClosed
  else match RequestExecutionLease.step? world.lease
      (.authorizeProducerDecision .mutationWriteGate generation .closeOrRetract) with
  | none => .error .leaseRejected
  | some lease => .ok { world with lease := lease, segments := world.segments ++ [record] }

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
      (match validateOpenPrefix segments closing.coordinate closing.writer with
      | .ok _ => true
      | .error _ => false) &&
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

def freshAuxiliaryExtentExact (segments : List Segment) (closing : Segment) : Bool :=
  match closing.close with
  | some (.closed .complete _ _) => freshCompleteExtentExact segments closing
  | some (.closed .«partial» count _) =>
      let data := sourceData segments closing.coordinate
      count == data.length &&
        (match validateOpenPrefix segments closing.coordinate closing.writer with
        | .ok _ => true
        | .error _ => false) &&
        timestampsNondecreasing data &&
        data.all (fun record => record.createdAt ≤ closing.createdAt)
  | _ => false

/-- Auxiliary capture closes under its request lease without accepting a
message or changing transcript/tool state. Compaction and fallback use the
parent request; title capture uses its separately admitted title request. The
native adapter must preserve the typed capture-scope kind. -/
def closeAuxiliaryCore (world : World) (generation : Generation)
    (closing : Segment) : Except Error World :=
  if segmentIdentityCollision world closing then .error .identityCollision
  else if closing ∈ world.segments then
    if closing.coordinate.source.isAuxiliary &&
        validateClosingRecord world.segments closing then .ok world
    else .error .invalidSegment
  else if requestSegmentShape world generation closing = false ||
      closing.coordinate.source.isAuxiliary = false ||
      closing.flush.isSome ||
      (closedComplete closing = false && closedPartial closing = false) then
    .error .invalidSegment
  else if sourceOpen world closing.coordinate = false then .error .sourceAlreadyClosed
  else
    let segments := world.segments ++ [closing]
    if validateClosingRecord segments closing = false ||
        freshAuxiliaryExtentExact segments closing = false then .error .invalidSegment
    else match RequestExecutionLease.step? world.lease
        (.authorizeProducerDecision .mutationWriteGate generation .closeOrRetract) with
    | none => .error .leaseRejected
    | some lease => .ok { world with lease := lease, segments := segments }

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

def acceptedPublicationPresent (world : World) (closing : Segment)
    (message : MessageEnvelope) : Bool :=
  let turn := messageTurn message
  closing ∈ world.segments && message ∈ world.messages &&
    publicationRowPresent world message.header turn &&
    turn.callIds.all (toolIntentPresent world turn)

def acceptedToolsPresent (world : World) (message : MessageEnvelope) : Bool :=
  (toolIntents message).all fun intent =>
    match ownedToolByDocument? world intent.call with
    | some tool => tool.requestDoc == world.requestId &&
        tool.session == message.header.session &&
        tool.acceptedSequence == message.sequence
    | none => false

/-- Replay must present the same physical admissions and immutable genesis as
the accepted transaction. -/
def acceptedAdmissionsPresent (world : World) (admissions : List ToolAdmission) : Bool :=
  admissions.all fun admission =>
    match ownedToolByDocument? world admission.document with
    | some tool => ToolGenesis.fromContext tool.context ==
        ToolGenesis.fromContext admission.context
    | none => false

def acceptedReplayPresent (world : World) (closing : Segment)
    (message : MessageEnvelope) (admissions : List ToolAdmission) : Bool :=
  acceptedPublicationPresent world closing message &&
    acceptedToolsPresent world message && acceptedAdmissionsPresent world admissions

/-- Accepted provider turn transaction: validated Complete closure, typed native
message, assistant transcript row and every ordered pending tool intent appear
together before dispatch. -/
def acceptAndPublishCore (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope)
    (admissions : List ToolAdmission) :
    Except Error World :=
  let header := message.header
  let turn := messageTurn message
  if segmentIdentityCollision world closing ∨ messageIdentityCollision world message then
    .error .identityCollision
  else if acceptedReplayPresent world closing message admissions then
    if !messageIdentityCollision world message && closedComplete closing &&
        validateClosingRecord world.segments closing &&
        acceptedMessageValid world generation world.segments message &&
        acceptedSourceBound world generation closing message &&
        toolProjectionCoherent world then .ok world
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
    else if ¬ world.transcript.PublishableTurn turn ||
        admissionsValid world message admissions = false then .error .transcriptRejected
    else match RequestExecutionLease.step? world.lease
        (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish) with
    | none => .error .leaseRejected
    | some lease =>
        let candidate : World :=
          { world with
          lease := lease
          segments := segments
          messages := world.messages ++ [message]
          transcript := world.transcript.publishAcceptedAssistant header.id turn
          toolContexts := installAcceptedTools world message admissions }
        if toolProjectionCoherent candidate then .ok candidate
        else .error .transcriptRejected

set_option maxHeartbeats 1000000 in
/-- Successful acceptance is either identity replay or the single fresh atomic
post-state constructed by the core.  This is the common elimination lemma for
consumers of acceptance; they need not repeat its admission branch ladder. -/
theorem acceptAndPublishCore_success_effect
    (world post : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope)
    (admissions : List ToolAdmission)
    (h : acceptAndPublishCore world generation closing message admissions = .ok post) :
    (post = world ∧ toolProjectionCoherent post = true) ∨
      ∃ lease,
        world.transcript.PublishableTurn (messageTurn message) ∧
        post = { world with
          lease := lease
          segments := world.segments ++ [closing]
          messages := world.messages ++ [message]
          transcript := world.transcript.publishAcceptedAssistant
            message.header.id (messageTurn message)
          toolContexts := installAcceptedTools world message admissions } ∧
        toolProjectionCoherent post = true := by
  simp (config := { maxSteps := 1000000 }) [acceptAndPublishCore] at h
  by_cases hc : segmentIdentityCollision world closing = true ∨
      messageIdentityCollision world message = true
  · simp only [if_pos hc] at h
    contradiction
  · simp only [if_neg hc] at h
    by_cases hp : acceptedReplayPresent world closing message admissions = true
    · simp only [if_pos hp] at h
      split at h <;> try contradiction
      rename_i hvalid
      cases h
      exact Or.inl ⟨rfl, hvalid.2⟩
    · simp only [if_neg hp] at h
      repeat' (split at h <;> try contradiction)
      all_goals cases h
      all_goals first | exact Or.inr ⟨_, by simp_all, rfl, by assumption⟩

/-- Both replay and fresh publication establish the complete tool projection
inside the atomic core.  Callers need not recompute it after success. -/
theorem acceptAndPublishCore_success_toolProjectionCoherent
    (world post : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope)
    (admissions : List ToolAdmission)
    (h : acceptAndPublishCore world generation closing message admissions = .ok post) :
    toolProjectionCoherent post = true := by
  rcases acceptAndPublishCore_success_effect world post generation closing message
    admissions h with ⟨_, hcoherent⟩ | ⟨_, _, _, hcoherent⟩
  · exact hcoherent
  · exact hcoherent

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
    if !messageIdentityCollision world message && closedComplete closing &&
        validateClosingRecord world.segments closing &&
        authoredMessageValid world generation world.segments closing message then .ok world
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
    else match RequestExecutionLease.step? world.lease
        (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish) with
    | none => .error .leaseRejected
    | some lease => .ok
        { world with
          lease := lease
          segments := segments
          messages := world.messages ++ [message]
          transcript := appendAuthoredRow world.transcript message }

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
    (message : MessageEnvelope) (admissions : List ToolAdmission) : Except Error World :=
  if messageIdentityCollision world message then .error .identityCollision
  else if headerOnlyPublicationPresent world message && acceptedToolsPresent world message then
    if !messageIdentityCollision world message &&
        headerOnlyMessageValid world generation message &&
        toolProjectionCoherent world then .ok world
    else .error .publicationIncomplete
  else if freshMessageIdentity world message = false then .error .identityCollision
  else if message.createdAt != world.lease.now ||
      headerOnlyMessageValid world generation message = false then .error .invalidHeader
  else if world.transcript.sessionId != world.sessionId ||
      message.sequence != world.transcript.nextSeq then .error .transcriptRejected
  else if message.header.role == .assistant &&
      ¬ world.transcript.PublishableTurn (messageTurn message) then .error .transcriptRejected
  else if admissionsValid world message admissions = false then .error .transcriptRejected
  else match RequestExecutionLease.step? world.lease
      (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish) with
  | none => .error .leaseRejected
  | some lease =>
      let candidate : World :=
        { world with
        lease := lease
        messages := world.messages ++ [message]
        toolContexts := installAcceptedTools world message admissions
        transcript := appendHeaderOnlyRow world.transcript message }
      if toolProjectionCoherent candidate then .ok candidate
      else .error .transcriptRejected

set_option maxHeartbeats 1000000 in
/-- Header-only replay and fresh publication both establish the complete tool
projection in the core transaction. -/
theorem publishHeaderOnlyCore_success_toolProjectionCoherent
    (world post : World) (generation : Generation)
    (message : MessageEnvelope) (admissions : List ToolAdmission)
    (h : publishHeaderOnlyCore world generation message admissions = .ok post) :
    toolProjectionCoherent post = true := by
  simp only [publishHeaderOnlyCore] at h
  repeat' first
    | contradiction
    | (solve | cases h; simp_all)
    | split at h

def dispatchPublicationValid (world : World) (generation : Generation)
    (callId : ToolExecution.ToolCallId) : Bool :=
  let candidates := (world.messages.filter fun message =>
    message.header.session == world.sessionId && message.header.role == .assistant &&
      (toolIntents message).any (fun intent => intent.call == callId)).dedup
  match candidates with
  | [message] =>
      !messageIdentityCollision world message &&
        acceptedMessageValid world generation world.segments message &&
        publicationRowPresent world message.header (messageTurn message) &&
        toolIntentPresent world (messageTurn message) callId &&
        match targetIntent message callId with
        | none => false
        | some intent => match resolveClose world.segments noDeniedDocuments intent.arguments with
          | .error _ => false
          | .ok closing => closedComplete closing &&
              validateClosingRecord world.segments closing &&
              acceptedSourceBound world generation closing message
  | _ => false

def toolDispatchPublicationValid (world : World) (generation : Generation)
    (document : DocId) : Bool :=
  match ownedToolByDocument? world document with
  | some tool => match tool.provenance with
    | .acceptedIntent => dispatchPublicationValid world generation document
    | .spawnedBackground _ => acceptedHeaderBindsToolGeneration world tool generation
  | none => false

def toolReadyToDispatch (world : World) (document : DocId) : Bool :=
  match ownedToolByDocument? world document with
  | some tool => match tool.provenance with
    | .acceptedIntent => decide (world.transcript.ReadyToDispatch document)
    | .spawnedBackground _ => tool.context.state == .pending
  | none => false

def dispatchCore (world : World) (generation : Generation)
    (permit : DispatchPermit) : Except Error World :=
  let callId := permit.call
  if toolDispatchPublicationValid world generation callId = false then .error .publicationIncomplete
  else if physicalRunning world callId then .ok world
  else if !permit.cancellationAllows || !permit.toolPolicyAllows then .error .transcriptRejected
  else if toolReadyToDispatch world callId = false ||
      executionAdmitted world callId = false then .error .transcriptRejected
  else match ownedToolByDocument? world callId with
  | none => .error .transcriptRejected
  | some tool =>
      if world.lease.now < tool.context.currentTime then .error .transcriptRejected
      else
      let current := { tool.context with currentTime := world.lease.now }
      match ToolExecution.ToolCallContext.step? current .dispatch with
      | none => .error .transcriptRejected
      | some context =>
          match RequestExecutionLease.step? world.lease
              (.authorizeProducerDecision .mutationWriteGate generation .dispatch) with
          | none => .error .leaseRejected
          | some lease =>
              let updated := { tool with context := context }
              .ok { world with
                lease := lease
                toolContexts := replaceOwnedTool world.toolContexts callId updated
                transcript := world.transcript.dispatchToolCallWithMode
                  callId context.awaitMode }

def toolControlAction : ToolExecution.ToolCallContext.Action → Bool
  | .background | .foreground => true
  | _ => false

/-- Explicit mode control for the exact accepted physical tool. It is
generation-fenced through the request owner, and foreground reacquisition is
forbidden after durable handoff or parent terminalization. -/
def changeToolControlCore (world : World) (generation : Generation) (document : DocId)
    (action : ToolExecution.ToolCallContext.Action) : Except Error World :=
  if toolControlAction action = false then .error .transcriptRejected
  else match ownedToolByDocument? world document with
  | none => .error .transcriptRejected
  | some tool =>
      if acceptedHeaderBindsToolGeneration world tool generation = false ||
          tool.stuckSince.isSome then
        .error .transcriptRejected
      else if action == .foreground && world.terminalSelection.isSome then
        .error .terminalRejected
      else if action == .foreground && tool.provenance != .acceptedIntent then
        .error .transcriptRejected
      else match ToolExecution.ToolCallContext.step? tool.context action with
      | none => .error .transcriptRejected
      | some context =>
          match RequestExecutionLease.step? world.lease
              (.authorizeProducerDecision .mutationWriteGate generation .dispatch) with
          | none => .error .leaseRejected
          | some lease =>
              let updated := { tool with context := context }
              let transcript := match action with
                | .background => world.transcript.releaseParentInFlight document
                | .foreground => world.transcript.claimParentInFlight document
                | _ => world.transcript
              .ok { world with
                lease := lease
                toolContexts := replaceOwnedTool world.toolContexts document updated
                transcript := transcript }

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
  | ⟨_, .auxiliary _ _ _ _⟩ => true
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
                  !messageIdentityCollision world message &&
                  publicationRowPresent world message.header (messageTurn message)

def terminalReplayPresent (world : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) : Bool :=
  world.lease.lease == .terminal generation outcome &&
    world.terminalSelection == some selection

def acceptedOwnedCalls (world : World) (generation : Generation) :
    List (SessionId × Transcript.Sequence × ToolExecution.ToolCallId) :=
  world.messages.flatMap fun message =>
    match message.header.publication with
    | .requestExecution owner =>
        if owner == generation && acceptedMessageValid world owner world.segments message &&
            publicationRowPresent world message.header (messageTurn message) then
          (toolIntents message).map (fun intent =>
            (message.header.session, message.sequence, intent.call))
        else []
    | _ => []

def ownedByGeneration (world : World) (generation : Generation) (tool : OwnedTool) : Bool :=
  acceptedHeaderBindsToolGeneration world tool generation

def normalCompletionToolsReady (world : World) (generation : Generation) : Bool :=
  world.toolContexts.all fun tool =>
    if ownedByGeneration world generation tool then
      match tool.provenance, tool.context.state with
      | _, .pending => true
      | .spawnedBackground _, .running => tool.context.awaitMode == .background
      | .acceptedIntent, .running =>
          tool.context.awaitMode == .background && canonicalToolDelivered world tool
      | .spawnedBackground _, terminal =>
          tool.context.awaitMode == .background && decide (isTerminal terminal)
      | .acceptedIntent, terminal => tool.context.startedAt.isNone ||
          (decide (isTerminal terminal) && canonicalToolDelivered world tool)
    else true

/-- A running call outlives its request's exceptional terminal: the handoff
records uncertainty and never cancels. A request terminal is not a cancel
signal for a session that request started; its background row keeps running
and later delivers that session's result as a completion notification. -/
def handoffRunningTool (world : World) (tool : OwnedTool) : OwnedTool :=
  { tool with stuckSince := some world.lease.now }

def accountOneOwnedTool (world : World) (generation : Generation)
    (interruptRunning : Bool) (tool : OwnedTool) : OwnedTool × Transcript.TranscriptState :=
  if !ownedByGeneration world generation tool then (tool, world.transcript)
  else match tool.context.state with
  | .pending =>
      match ToolExecution.ToolCallContext.step? tool.context
          (.cancelBeforeDispatch .interrupted) with
      | none => (tool, world.transcript)
      | some context =>
          ({ tool with context := context },
            world.transcript.terminalizeToolCall tool.document .cancelled)
  | .running =>
      let accounted := if interruptRunning then handoffRunningTool world tool else tool
      (accounted, world.transcript.releaseParentInFlight tool.document)
  | _ => (tool, world.transcript.releaseParentInFlight tool.document)

def accountOwnedTools (world : World) (generation : Generation)
    (interruptRunning : Bool) : World :=
  world.toolContexts.foldl (fun current original =>
    let (tool, transcript) := accountOneOwnedTool current generation interruptRunning original
    { current with
      toolContexts := replaceOwnedTool current.toolContexts original.document tool
      transcript := transcript }) world

def accountOneMetadataOwnedTool (world : World) (generation : Generation)
    (tool : OwnedTool) : OwnedTool × Transcript.TranscriptState :=
  if !metadataOwnedByGeneration world generation tool then (tool, world.transcript)
  else match tool.context.state with
  | .pending =>
      match ToolExecution.ToolCallContext.step? tool.context
          (.cancelBeforeDispatch .interrupted) with
      | none => (tool, world.transcript)
      | some context => ({ tool with context := context },
          world.transcript.terminalizeToolCall tool.document .cancelled)
  | .running =>
      (handoffRunningTool world tool,
        world.transcript.releaseParentInFlight tool.document)
  | _ => (tool, world.transcript.releaseParentInFlight tool.document)

def accountMetadataOwnedTools (world : World) (generation : Generation) : World :=
  world.toolContexts.foldl (fun current original =>
    let (tool, transcript) := accountOneMetadataOwnedTool current generation original
    { current with
      toolContexts := replaceOwnedTool current.toolContexts original.document tool
      transcript := transcript }) world

def ownedPendingSettled (world : World) (generation : Generation) : Bool :=
  world.transcript.toolCalls.all fun row =>
    if (row.sessionId, row.messageSequence, row.callId) ∈ acceptedOwnedCalls world generation then
      row.state != .pending
    else true

def preparedRecoveryWorld (world : World) (prepared : RecoveryPrepared)
    (expected : Generation) : World :=
  accountOwnedTools
    { world with
      segments := prepared.segments
      messages := prepared.messages
      transcript := prepared.transcript }
    expected true

/-- A live producer may atomically retain the text prefix of one provider
source under its current generation before terminal request CAS. This reuses
the recovery closure/header validator, but neither swaps generation nor
accounts tools: it is the same producer closing its own attempt. -/
def closePartialAndPublishCore (world : World) (generation : Generation)
    (item : RecoveryItem) : Except Error World :=
  if recoveryReplayValid world generation generation [item] then .ok world
  else match prepareRecoveryItems world generation generation [item]
      ⟨world.segments, world.messages, world.transcript⟩ with
  | .error error => .error error
  | .ok prepared =>
      match RequestExecutionLease.step? world.lease
          (.authorizeProducerDecision .mutationWriteGate generation .closeOrRetract) with
      | none => .error .leaseRejected
      | some lease => .ok { world with
          lease := lease
          segments := prepared.segments
          messages := prepared.messages
          transcript := prepared.transcript }

/-- One gate transaction closes every unresolved keyed provider source, accounts
every exact old-generation tool document, and only then swaps generation.
Running effects remain running; durable cancellation/reconcile intent releases
the crashed parent's hook without claiming a host acknowledgement. -/
def recoverExpiredBatchCore (world : World) (expected fresh : Generation)
    (duration deadline : Time) (items : List RecoveryItem) : Except Error World :=
  if recoveryReplayValid world expected fresh items then
    .ok world
  else match prepareRecoveryBatch world expected fresh items with
  | .error error => .error error
  | .ok prepared =>
      let accounted := preparedRecoveryWorld world prepared expected
      match RequestExecutionLease.step? accounted.lease
          (.recoverExpired .mutationWriteGate expected fresh duration deadline) with
      | none => .error .leaseRejected
      | some lease => .ok
          { accounted with lease := lease }

/-- Recovery publication, old-generation tool accounting, terminal lease CAS,
and exact terminal selection form one transaction. This is distinct from the
resumable `recoverExpiredBatchCore` transition. The caller supplies the
selection; the execution owner validates it against the repaired facts. -/
def recoverExpiredTerminalCore (world : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (items : List RecoveryItem) : Except Error World :=
  if terminalReplayPresent world fresh outcome selection then .ok world
  else if world.terminalSelection.isSome then .error .terminalRejected
  else match prepareRecoveryBatch world expected fresh items with
  | .error error => .error error
  | .ok prepared =>
      let accounted := preparedRecoveryWorld world prepared expected
      if terminalSelectionValid accounted selection = false then .error .terminalRejected
      else match RequestExecutionLease.step? accounted.lease
          (.recoverExpiredTerminal .mutationWriteGate expected fresh outcome) with
      | none => .error .leaseRejected
      | some lease => .ok
          { accounted with lease := lease, terminalSelection := some selection }

/-- Final request lifecycle and exact terminal-output selection commit together.
No latest-message fallback is available. -/
def terminalizeCore (world : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) :
    Except Error World :=
  if terminalSelectionValid world selection = false then .error .terminalRejected
  else if terminalReplayPresent world generation outcome selection then .ok world
  else if world.terminalSelection.isSome then .error .terminalRejected
  else if outcome == .completed && normalCompletionToolsReady world generation = false then
    .error .terminalRejected
  else
    let accounted := accountOwnedTools world generation (outcome != .completed)
    match RequestExecutionLease.step? accounted.lease
      (.finalize .mutationWriteGate generation outcome) with
  | none => .error .leaseRejected
  | some lease => .ok
      { accounted with
        lease := lease
        terminalSelection := some selection }

/-- Escape from post-acceptance payload corruption. This path deliberately
uses only immutable header/tool provenance and the policy-revocation lease
action; it never repairs, chooses, or closes conflicting output bytes. -/
def revokeCorruptCore (world : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) :
    Except Error World :=
  if terminalReplayPresent world fresh outcome selection then .ok world
  else if world.terminalSelection.isSome ||
      terminalSelectionMetadataValid world selection = false then .error .terminalRejected
  else
    let accounted := accountMetadataOwnedTools world expected
    match RequestExecutionLease.step? accounted.lease
        (.policyRevoke .mutationWriteGate expected fresh outcome) with
    | none => .error .leaseRejected
    | some lease => .ok { accounted with lease := lease, terminalSelection := some selection }

def appendRaw (world : World) (generation : Generation)
    (record : Segment) : Except Error World :=
  appendRawCore world generation record

def closeAuxiliary (world : World) (generation : Generation)
    (closing : Segment) : Except Error World :=
  checked (fun post =>
    closing ∈ post.segments &&
      (closures post.segments closing.coordinate).dedup == [closing] &&
      validateClosingRecord post.segments closing)
    (closeAuxiliaryCore world generation closing)

theorem closeAuxiliary_success_effect (before after : World)
    (generation : Generation) (closing : Segment)
    (h : closeAuxiliary before generation closing = .ok after) :
    after = before ∨
      ∃ lease, after = { before with lease := lease, segments := before.segments ++ [closing] } := by
  have hc := checked_core_success _ _ _ h
  unfold closeAuxiliaryCore at hc
  repeat' first
    | split at hc
    | contradiction
    | (solve | cases hc; exact Or.inl rfl)
  all_goals
    dsimp only at hc
    split at hc <;> simp_all
  all_goals exact Or.inr ⟨_, hc.symm⟩

def retractBeforeRetry (world : World) (generation : Generation)
    (record : Segment) : Except Error World :=
  checked (fun post =>
    record ∈ post.segments &&
      (closures post.segments record.coordinate).dedup == [record])
    (retractBeforeRetryCore world generation record)

def acceptAndPublish (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope)
    (admissions : List ToolAdmission) :
    Except Error World :=
  checked (fun post =>
    acceptedPublicationPresent post closing message && acceptedToolsPresent post message &&
      validateClosingRecord post.segments closing &&
      acceptedMessageValid post generation post.segments message &&
      acceptedSourceBound post generation closing message)
    (acceptAndPublishCore world generation closing message admissions)

def dispatch (world : World) (generation : Generation)
    (permit : DispatchPermit) : Except Error World :=
  checked (fun post => physicalRunning post permit.call &&
    toolDispatchPublicationValid post generation permit.call && toolProjectionCoherent post)
    (dispatchCore world generation permit)

/-- The running accepted `spawn_process` owner creates a distinct childless
background lifecycle row. It inherits exact request/session/sequence and
generation ownership from that physical parent, but creates no assistant tool
intent or transcript tool-call row of its own. -/
def admitSpawnedBackgroundCore (world : World) (generation : Generation)
    (admission : SpawnedToolAdmission) : Except Error World :=
  if spawnedAdmissionReplayValid world admission then .ok world
  else if world.toolContexts.any (fun tool =>
      tool.provenance == .spawnedBackground admission.parentToolDoc) then
    .error .transcriptRejected
  else if spawnedAdmissionValid world generation admission = false then
    .error .transcriptRejected
  else match installSpawnedTool world admission with
  | none => .error .transcriptRejected
  | some spawned =>
      match RequestExecutionLease.step? world.lease
          (.authorizeProducerDecision .mutationWriteGate generation .dispatch) with
      | none => .error .leaseRejected
      | some lease =>
          let candidate := { world with
            lease := lease
            toolContexts := world.toolContexts ++ [spawned] }
          if toolProjectionCoherent candidate then .ok candidate
          else .error .transcriptRejected

def admitSpawnedBackground (world : World) (generation : Generation)
    (admission : SpawnedToolAdmission) : Except Error World :=
  checked (fun post => toolProjectionCoherent post && spawnedToolPresent post admission)
    (admitSpawnedBackgroundCore world generation admission)

def changeToolControl (world : World) (generation : Generation) (document : DocId)
    (action : ToolExecution.ToolCallContext.Action) : Except Error World :=
  checked toolProjectionCoherent
    (changeToolControlCore world generation document action)

def publishAuthored (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) : Except Error World :=
  checked (fun post =>
    authoredPublicationPresent post closing message &&
      validateClosingRecord post.segments closing &&
      authoredMessageValid post generation post.segments closing message)
    (publishAuthoredCore world generation closing message)

def publishHeaderOnly (world : World) (generation : Generation)
    (message : MessageEnvelope) (admissions : List ToolAdmission) : Except Error World :=
  checked (fun post =>
    headerOnlyPublicationPresent post message &&
      headerOnlyMessageValid post generation message && acceptedToolsPresent post message)
    (publishHeaderOnlyCore world generation message admissions)

def recoverExpiredBatch (world : World) (expected fresh : Generation)
    (duration deadline : Time) (items : List RecoveryItem) :
    Except Error World :=
  checked (fun post =>
    recoveryBatchPresent post fresh items &&
      toolProjectionCoherent post &&
      (recoveryReplayValid world expected fresh items ||
        recoveryCoversAllSources world expected items) &&
      items.all (fun item =>
        (closures post.segments item.closing.coordinate).dedup == [item.closing] &&
          item.closing.writer == .request expected &&
          (recoveryReplayValid world expected fresh items ||
            recoveryExtentExact world expected item.closing)))
    (recoverExpiredBatchCore world expected fresh duration deadline items)

def recoverExpiredTerminal (world : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (items : List RecoveryItem) : Except Error World :=
  checked (fun post =>
    terminalReplayPresent post fresh outcome selection &&
      terminalSelectionValid post selection && toolProjectionCoherent post &&
      (terminalReplayPresent world fresh outcome selection ||
        recoveryCoversAllSources world expected items) &&
      items.all (fun item =>
        (closures post.segments item.closing.coordinate).dedup == [item.closing] &&
          item.closing.writer == .request expected &&
          (terminalReplayPresent world fresh outcome selection ||
            recoveryExtentExact world expected item.closing)))
    (recoverExpiredTerminalCore world expected fresh outcome selection items)

def closePartialAndPublish (world : World) (generation : Generation)
    (item : RecoveryItem) : Except Error World :=
  checked (fun post =>
    recoveryItemPresent post item &&
      (closures post.segments item.closing.coordinate).dedup == [item.closing] &&
      item.closing.writer == .request generation &&
      toolProjectionCoherent post &&
      (recoveryReplayValid world generation generation [item] ||
        recoveryExtentExact world generation item.closing))
    (closePartialAndPublishCore world generation item)

def terminalize (world : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) :
    Except Error World :=
  checked (fun post =>
    terminalReplayPresent post generation outcome selection &&
      terminalSelectionValid post selection && ownedPendingSettled post generation &&
      toolProjectionCoherent post)
    (terminalizeCore world generation outcome selection)

def revokeCorrupt (world : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection) :
    Except Error World :=
  checked (fun post =>
    terminalReplayPresent post fresh outcome selection &&
      terminalSelectionMetadataValid post selection &&
      post.segments == world.segments && post.messages == world.messages)
    (revokeCorruptCore world expected fresh outcome selection)

end CanonicalOutput.Execution
