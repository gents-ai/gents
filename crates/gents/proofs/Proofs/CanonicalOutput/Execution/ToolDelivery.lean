import Proofs.CanonicalOutput.Execution.Transition
import Proofs.CanonicalOutput.ToolDelivery
import Proofs.Background.Executable

/-!
# Tool delivery in the shared execution world

Tool lifecycle, canonical segments/messages, and transcript allocation are
updated in one `Execution.World`. None of these transitions reads or rewrites
the parent request lease. The gate supplies a monotonic owner time by updating
`world.lease.now`; background delivery therefore remains possible after the
parent request is terminal without reviving it.
-/
namespace CanonicalOutput.Execution.ToolDelivery

inductive Error where
  | missingTool
  | ownership
  | clock
  | lifecycle
  | source (error : CanonicalOutput.ToolDelivery.Error)
  | publication
  | cursor
  deriving DecidableEq, Repr

inductive CloseAuthority where
  /-- A confirmed native lifecycle result. A cancellation request by itself is
  not this evidence; `.cancelDuringRun` means the host stop was confirmed. -/
  | native (action : ToolExecution.ToolCallContext.Action)
  /-- A terminal lifecycle already committed by the cancellation/recovery
  owner, including never-dispatched pending cancellation. -/
  | alreadyTerminal
  /-- Reuse the executable child bridge owner rather than accepting a raw
  caller-supplied child-complete Boolean. -/
  | bridge (state : Subagent.BridgedState) (event : Subagent.BridgedState.Event)

def resultKey (document : DocId) (message : MessageEnvelope) : Transcript.ToolResultKey :=
  { sessionId := message.header.session
  , logicalResultId := document
  , payloadHash := message.header.id }

def cursorAllowsFresh (world : World) (sequence : Transcript.Sequence) : Bool :=
  match world.compactionCursor with
  | none => true
  | some cursor => cursor < sequence

def updateClock (world : World) (tool : OwnedTool) : Option OwnedTool :=
  if tool.context.currentTime ≤ world.lease.now then
    some { tool with context := { tool.context with currentTime := world.lease.now } }
  else none

def bindingValid (world : World) (tool : OwnedTool) : Bool :=
  tool.session == world.sessionId &&
    (world.toolContexts.filter (fun candidate => candidate.document == tool.document)).length == 1 &&
    acceptedHeaderBindsTool world tool

def bridgeTerminalContext? (tool : OwnedTool) (state : Subagent.BridgedState)
    (event : Subagent.BridgedState.Event) : Option ToolExecution.ToolCallContext := do
  let (_, before) ← Subagent.BridgedState.findBridgeSlot?
    state.parent.tools state.bridgeCallId
  if before != tool.context then none
  let after ← Subagent.BridgedState.step state event
  let (_, terminal) ← Subagent.BridgedState.findBridgeSlot?
    after.parent.tools state.bridgeCallId
  if terminal.callId != tool.context.callId ||
      !CanonicalOutput.ToolDelivery.terminalState terminal.state then none
  else some terminal

def terminalContext? (world : World) (tool : OwnedTool)
    (authority : CloseAuthority) : Option ToolExecution.ToolCallContext := do
  let observed ← updateClock world tool
  let terminal ← match authority with
    | .native action =>
        if observed.context.childRequestId.isSome then none
        else ToolExecution.ToolCallContext.step? observed.context action
    | .alreadyTerminal => some observed.context
    | .bridge state event => do
        let projected ← bridgeTerminalContext? tool state event
        if projected.currentTime ≤ world.lease.now then
          some { projected with currentTime := world.lease.now }
        else none
  if CanonicalOutput.ToolDelivery.terminalState terminal.state then some terminal else none

def clearReconcileIntent (tool : OwnedTool)
    (context : ToolExecution.ToolCallContext) : OwnedTool :=
  { tool with
    context := context
    cancelCascadeIntentAt := none
    cancelPendingRemoteAck := false
    stuckSince := none }

/-- Tool output uses the tool lifecycle only. It cannot revive or extend the
request lease, including when the parent is already terminal. This hot path
checks the exact indexed tool binding and its source prefix; it does not rescan
every other accepted header in the session. Native storage refines this lookup
through its immutable document index and transaction gate. -/
def appendToolOutput (world : World) (document : DocId)
    (record : Segment) : Except Error World :=
  match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if record ∈ world.segments &&
          CanonicalOutput.ToolDelivery.appendReplayValid world.segments
            tool.requestDoc tool.document record then .ok world
      else match updateClock world tool with
      | none => .error .clock
      | some observed =>
          if observed.context.state != .running || observed.context.deadlineExceeded then
            .error .lifecycle
          else match CanonicalOutput.ToolDelivery.appendRecords world.segments
              tool.requestDoc tool.document world.lease.now record with
          | .error error => .error (.source error)
          | .ok segments =>
              let post := { world with
                segments := segments
                toolContexts := replaceOwnedTool world.toolContexts document observed }
              .ok post

def closeReplayValid (world : World) (tool : OwnedTool) (record : Segment) : Bool :=
  CanonicalOutput.ToolDelivery.terminalState tool.context.state &&
    record ∈ world.segments &&
    CanonicalOutput.ToolDelivery.closedRecordValid world.segments
      tool.requestDoc tool.document record &&
    match tool.provenance with
    | .acceptedIntent =>
        match transcriptToolByDocument? world tool.document with
        | some row => row.state == tool.context.state &&
            decide (tool.document ∉ world.transcript.inFlight)
        | none => false
    | .spawnedBackground _ =>
        (transcriptToolByDocument? world tool.document).isNone &&
          decide (tool.document ∉ world.transcript.inFlight)

/-- Closure and confirmed native terminal state commit together. The transcript
row receives that exact terminal state and leaves `inFlight`; result pairing is
added only by `publishToolDelivery`. -/
def closeToolOutput (world : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment) : Except Error World :=
  if !toolLifecycleProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if closeReplayValid world tool record then .ok world
      else match terminalContext? world tool authority with
      | none => .error .lifecycle
      | some context =>
          match CanonicalOutput.ToolDelivery.closeRecords world.segments
              tool.requestDoc tool.document world.lease.now record with
          | .error error => .error (.source error)
          | .ok segments =>
              let updated := clearReconcileIntent tool context
              let post := { world with
                segments := segments
                toolContexts := replaceOwnedTool world.toolContexts document updated
                transcript := world.transcript.terminalizeToolCall document context.state }
              if !toolLifecycleProjectionCoherent post then .error .ownership else .ok post

def matchingMessageIdentity (left right : MessageEnvelope) : Bool :=
  left.header.id == right.header.id ||
    (left.header.session == right.header.session &&
      (left.key == right.key || left.sequence == right.sequence))

inductive DeliveryShape where
  | foregroundResult
  | backgroundNotification
  deriving DecidableEq, Repr

/-- Native result receipts and background completion notifications have
different transcript projections. A result row represents exactly one native
tool-result block. A notification is ordinary user content and may be
payload-free (the pending-cancelled acknowledgement case). -/
def deliveryShape? (message : MessageEnvelope) (document : DocId) : Option DeliveryShape :=
  match message.blocks with
  | [.toolResult call _ _ _] =>
      if call == document then some .foregroundResult else none
  | blocks =>
      if blocks.all (fun block => match block with | .text _ => true | _ => false)
      then some .backgroundNotification else none

def resultProviderMatches (world : World) (tool : OwnedTool)
    (message : MessageEnvelope) : Bool :=
  match message.blocks with
  | [.toolResult call providerId _ _] =>
      call == tool.document && acceptedProviderId? world tool == some providerId
  | _ => true

/-- A background completion notification is insert-once per physical tool.
The immediate singleton result receipt is a separate foreground-result shape
and therefore does not consume this slot. -/
def terminalNotificationExists (world : World) (tool : OwnedTool) : Bool :=
  world.messages.any fun message =>
    message.header.publication == .toolDelivery tool.document &&
      message.header.session == tool.session &&
      deliveryShape? message tool.document == some .backgroundNotification

def referenceOwnedByTool (world : World) (tool : OwnedTool)
    (reference : PayloadRef) : Bool :=
  match resolveClose world.segments noDeniedDocuments reference with
  | .error _ => false
  | .ok closing =>
      closing.coordinate == CanonicalOutput.ToolDelivery.coordinate
        tool.requestDoc tool.document &&
      closing.writer == .tool tool.document &&
      CanonicalOutput.ToolDelivery.closedRecordValid world.segments
        tool.requestDoc tool.document closing

def deliveryHeaderValid (world : World) (tool : OwnedTool)
    (message : MessageEnvelope) : Bool :=
  message.header.publication == .toolDelivery tool.document &&
    message.header.request == some tool.requestDoc &&
    message.header.session == tool.session && message.header.role == .user &&
    CanonicalOutput.ToolDelivery.terminalState tool.context.state &&
    CanonicalOutput.ToolDelivery.sourceClosed world.segments tool.requestDoc tool.document &&
    (deliveryShape? message tool.document).isSome &&
    resultProviderMatches world tool message &&
    (envelopeRefs message).all (referenceOwnedByTool world tool) &&
    match reconstructMessage world.segments [] message with
    | .ok _ => true
    | .error _ => false

def wakeBindingValid (world : World) (tool : OwnedTool)
    (binding : WakeDocumentBinding) (message : MessageEnvelope) : Bool :=
  binding.authenticated && binding.agent == world.principal &&
    binding.session == world.sessionId && binding.session == tool.session &&
    binding.entry.source == .backgroundCompletion &&
    decide (binding.entry.coalesceWellFormed binding.session) &&
    binding.wakeDocument != tool.requestDoc &&
    binding.notificationMessageId == message.header.id &&
    binding.notificationSequence == message.sequence &&
    message.header.request == some binding.wakeDocument

def wakeNotificationHeaderValid (world : World) (tool : OwnedTool)
    (binding : WakeDocumentBinding) (message : MessageEnvelope) : Bool :=
  wakeBindingValid world tool binding message &&
    tool.context.awaitMode == .background &&
    message.header.publication == .toolDelivery tool.document &&
    message.header.session == tool.session && message.header.role == .user &&
    message.header.outcome == .complete &&
    CanonicalOutput.ToolDelivery.terminalState tool.context.state &&
    CanonicalOutput.ToolDelivery.sourceClosed world.segments tool.requestDoc tool.document &&
    deliveryShape? message tool.document == some .backgroundNotification &&
    (envelopeRefs message).all (referenceOwnedByTool world tool) &&
    match reconstructMessage world.segments [] message with
    | .ok _ => true
    | .error _ => false

def goalBindingValid (world : World) (tool : OwnedTool)
    (binding : GoalNotificationBinding) (message : MessageEnvelope) : Bool :=
  binding.authenticated && binding.agent == world.principal &&
    binding.session == world.sessionId && binding.session == tool.session &&
    binding.parentRequestDocument == tool.requestDoc &&
    message.header.request == some binding.parentRequestDocument

def goalNotificationHeaderValid (world : World) (tool : OwnedTool)
    (binding : GoalNotificationBinding) (message : MessageEnvelope) : Bool :=
  goalBindingValid world tool binding message &&
    tool.context.awaitMode == .background &&
    deliveryShape? message tool.document == some .backgroundNotification &&
    deliveryHeaderValid world tool message

def deliveryRowPresent (world : World) (tool : OwnedTool)
    (message : MessageEnvelope) : Bool :=
  match deliveryShape? message tool.document with
  | none => false
  | some .foregroundResult =>
      let key := resultKey tool.document message
      world.transcript.messages.any (fun row =>
        row.messageId == message.header.id && row.sessionId == tool.session &&
          row.sequence == message.sequence && row.role == .user &&
          row.kind == .toolResult tool.document key) &&
        match transcriptToolByDocument? world tool.document with
        | some row => row.state == tool.context.state && row.resultKey == some key &&
            decide (tool.document ∉ world.transcript.inFlight)
        | none => false
  | some .backgroundNotification =>
      world.transcript.messages.any (fun row =>
        row.messageId == message.header.id && row.sessionId == tool.session &&
          row.sequence == message.sequence && row.role == .user &&
          row.kind == .ordinary) &&
        match tool.provenance with
        | .acceptedIntent =>
            match transcriptToolByDocument? world tool.document with
            | some row => row.state == tool.context.state &&
                decide (tool.document ∉ world.transcript.inFlight)
            | none => false
        | .spawnedBackground _ =>
            (transcriptToolByDocument? world tool.document).isNone &&
              decide (tool.document ∉ world.transcript.inFlight)

def publicationReplayValid (world : World) (tool : OwnedTool)
    (message : MessageEnvelope) : Bool :=
  message ∈ world.messages &&
    (world.messages.filter (matchingMessageIdentity message)).all (fun old => old == message) &&
    deliveryHeaderValid world tool message && deliveryRowPresent world tool message

private def notificationReplayValid
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (world : World) (tool : OwnedTool) (message : MessageEnvelope) : Bool :=
  message ∈ world.messages &&
    (world.messages.filter (matchingMessageIdentity message)).all (fun old => old == message) &&
    headerValid world tool message && deliveryRowPresent world tool message

def notificationReady (world : World) (tool : OwnedTool) : Bool :=
  !terminalNotificationExists world tool &&
    match tool.provenance with
    | .acceptedIntent =>
        match transcriptToolByDocument? world tool.document with
        | some row => row.state == tool.context.state &&
            decide (tool.document ∉ world.transcript.inFlight)
        | none => false
    | .spawnedBackground _ =>
        (transcriptToolByDocument? world tool.document).isNone

def freshDeliveryTranscript? (world : World) (tool : OwnedTool)
    (message : MessageEnvelope) : Option Transcript.TranscriptState :=
  match deliveryShape? message tool.document with
  | none => none
  | some .foregroundResult =>
      match tool.provenance, transcriptToolByDocument? world tool.document with
      | .acceptedIntent, some row =>
          let key := resultKey tool.document message
          if row.state != tool.context.state || row.resultKey.isSome ||
              world.transcript.hasToolResultKey key then none
          else some (world.transcript.publishToolResult
            tool.document message.header.id key tool.context.state)
      | _, _ => none
  | some .backgroundNotification =>
      if notificationReady world tool then some
      (world.transcript.appendUserMessage message.header.id .ordinary)
      else none

theorem background_notification_uses_ordinary_append
    (world : World) (tool : OwnedTool) (message : MessageEnvelope)
    (shape : deliveryShape? message tool.document = some .backgroundNotification)
    (ready : notificationReady world tool = true) :
    freshDeliveryTranscript? world tool message =
      some (world.transcript.appendUserMessage message.header.id .ordinary) := by
  simp [freshDeliveryTranscript?, shape, ready]

set_option maxHeartbeats 500000 in
theorem fresh_delivery_advances_allocator
    (world : World) (tool : OwnedTool) (message : MessageEnvelope)
    (transcript : Transcript.TranscriptState)
    (h : freshDeliveryTranscript? world tool message = some transcript) :
    transcript.nextSeq = world.transcript.nextSeq + 1 := by
  unfold freshDeliveryTranscript? at h
  split at h
  · contradiction
  · split at h <;> simp_all [Transcript.TranscriptState.publishToolResult]
    rcases h with ⟨⟨_, keyMissing⟩, transcriptEq⟩
    have seqEq := congrArg Transcript.TranscriptState.nextSeq transcriptEq
    simpa [keyMissing] using seqEq.symm
  · split at h <;>
      simp_all [Transcript.TranscriptState.appendUserMessage]
    rw [← h]

def receiptReferenceOwned (world : World) (tool : OwnedTool) (closing : Segment)
    (reference : PayloadRef) : Bool :=
  match resolveClose world.segments noDeniedDocuments reference with
  | .error _ => false
  | .ok resolved =>
      resolved == closing && resolved.coordinate.request == tool.requestDoc &&
      (match resolved.coordinate.source with | .authored _ => true | _ => false) &&
      resolved.writer == .tool tool.document &&
      match resolved.close with | some (.closed .complete _ _) => true | _ => false

def backgroundReceiptRecordValid (world : World) (tool : OwnedTool)
    (closing : Segment) : Bool :=
  closing.coordinate.request == tool.requestDoc &&
    (match closing.coordinate.source with | .authored _ => true | _ => false) &&
    closing.writer == .tool tool.document && closing.createdAt == world.lease.now &&
    freshSegmentIdentity world closing && sourceOpen world closing.coordinate &&
    validateClosingRecord (world.segments ++ [closing]) closing &&
    freshCompleteExtentExact (world.segments ++ [closing]) closing &&
    match closing.close with | some (.closed .complete _ _) => true | _ => false

/-- The immediate native receipt for a background child belongs to the direct
accepted parent call. It is reconstructed from a separate authored source
owned by that physical tool document; it neither closes nor terminalizes the
long-running child source. -/
def backgroundReceiptHeaderValid (world : World) (tool : OwnedTool) (closing : Segment)
    (message : MessageEnvelope) : Bool :=
  tool.provenance == .acceptedIntent &&
    message.header.publication == .toolDelivery tool.document &&
    message.header.request == some tool.requestDoc &&
    message.header.session == tool.session && message.header.role == .user &&
    message.header.outcome == .complete &&
    deliveryShape? message tool.document == some .foregroundResult &&
    resultProviderMatches world tool message &&
    (envelopeRefs message).all (receiptReferenceOwned world tool closing) &&
    match reconstructMessage world.segments noDeniedDocuments message with
    | .ok _ => true
    | .error _ => false

def backgroundReceiptReplayValid (world : World) (tool : OwnedTool)
    (closing : Segment) (message : MessageEnvelope) : Bool :=
  closing ∈ world.segments && exactIdentityAt world.segments closing &&
    message ∈ world.messages &&
    (world.messages.filter (matchingMessageIdentity message)).all (fun old => old == message) &&
    backgroundReceiptHeaderValid world tool closing message &&
    let key := resultKey tool.document message
    world.transcript.messages.any (fun row =>
      row.messageId == message.header.id && row.sessionId == tool.session &&
        row.sequence == message.sequence && row.role == .user &&
        row.kind == .toolResult tool.document key) &&
      match transcriptToolByDocument? world tool.document with
      | some row => row.resultKey == some key &&
          decide (tool.document ∉ world.transcript.inFlight)
      | none => false

/-- Publish the immediate singleton native tool-result receipt for a direct
background invocation (native or linked child). The call remains physically running; only
its transcript result pairing and the shared message cursor advance. Final
completion is a later ordinary notification through the typed Goal or wake
publication boundary. -/
def publishBackgroundReceipt (world : World) (document : DocId)
    (closing : Segment) (message : MessageEnvelope) : Except Error World :=
  if !toolProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if backgroundReceiptReplayValid world tool closing message then .ok world
      else if !backgroundReceiptRecordValid world tool closing ||
          world.messages.any (matchingMessageIdentity message) then .error .publication
      else
        let staged := { world with segments := world.segments ++ [closing] }
        if !backgroundReceiptHeaderValid staged tool closing message ||
            message.createdAt != world.lease.now then .error .publication
        else match updateClock world tool with
        | none => .error .clock
        | some updated =>
          if updated.context.state != .running ||
              updated.context.awaitMode != .background ||
              updated.context.deadlineExceeded then .error .lifecycle
          else if message.sequence != world.transcript.nextSeq then .error .publication
          else if !cursorAllowsFresh world message.sequence then .error .cursor
          else match transcriptToolByDocument? world document with
          | none => .error .publication
          | some row =>
            let key := resultKey document message
            if row.state != .running || row.resultKey.isSome ||
                world.transcript.hasToolResultKey key then .error .publication
            else
              let transcript := world.transcript.publishToolResult
                document message.header.id key .running
              let post := { world with
                segments := staged.segments
                messages := world.messages ++ [message]
                transcript := transcript
                toolContexts := replaceOwnedTool world.toolContexts document updated }
              if !toolProjectionCoherent post then .error .ownership else .ok post

/-- Fresh publication allocates exactly `transcript.nextSeq` and derives its
result key from session, physical tool document, and immutable header identity.
Exact replay allocates nothing and is allowed even after compaction advanced. -/
def publishToolDelivery (world : World) (document : DocId)
    (message : MessageEnvelope) : Except Error World :=
  if !toolProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if deliveryShape? message document != some .foregroundResult then
        .error .publication
      else if publicationReplayValid world tool message then .ok world
      else if world.messages.any (matchingMessageIdentity message) then .error .publication
      else if !deliveryHeaderValid world tool message || message.createdAt != world.lease.now then
        .error .publication
      else if message.sequence != world.transcript.nextSeq then .error .publication
      else if !cursorAllowsFresh world message.sequence then .error .cursor
      else match updateClock world tool with
      | none => .error .clock
      | some updated =>
          match freshDeliveryTranscript? world tool message with
          | none => .error .publication
          | some transcript =>
              let post := { world with
                messages := world.messages ++ [message]
                transcript := transcript
                toolContexts := replaceOwnedTool world.toolContexts document updated }
              if !toolProjectionCoherent post then .error .ownership else .ok post

private def publishBackgroundNotificationWith
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (world : World) (document : DocId) (message : MessageEnvelope) : Except Error World :=
  if !toolProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if notificationReplayValid headerValid world tool message then .ok world
      else if world.messages.any (matchingMessageIdentity message) then .error .publication
      else if !headerValid world tool message || message.createdAt != world.lease.now then
        .error .publication
      else if message.sequence != world.transcript.nextSeq then .error .publication
      else if !cursorAllowsFresh world message.sequence then .error .cursor
      else match updateClock world tool with
      | none => .error .clock
      | some updated =>
          match freshDeliveryTranscript? world tool message with
          | none => .error .publication
          | some transcript =>
              let post := { world with
                messages := world.messages ++ [message]
                transcript := transcript
                toolContexts := replaceOwnedTool world.toolContexts document updated }
              if !toolProjectionCoherent post then .error .ownership else .ok post

/-- A non-Goal background notification is request-owned by the exact physical
wake document from the authenticated queue transaction. Its payload refs stay
bound to the parent tool source; the logical queue request id is never used as
document authority. Queue enqueue/replay is composed in `BackgroundContinuation`. -/
def publishWakeNotification (world : World) (document : DocId)
    (binding : WakeDocumentBinding) (message : MessageEnvelope) : Except Error World :=
  publishBackgroundNotificationWith
    (fun world tool message => wakeNotificationHeaderValid world tool binding message)
    world document message

/-- A canonical Goal owner consumes a parent-bound notification without a
background wake. The physical Goal binding is an authenticated native premise. -/
def publishGoalNotification (world : World) (document : DocId)
    (binding : GoalNotificationBinding) (message : MessageEnvelope) : Except Error World :=
  publishBackgroundNotificationWith
    (fun world tool message => goalNotificationHeaderValid world tool binding message)
    world document message

/-- Every canonical message in the session is behind the one shared allocator.
This deliberately does not require a transcript row for system messages. -/
def messageSequenceCoherent (world : World) : Bool :=
  world.messages.all fun message =>
    message.header.session != world.sessionId ||
      message.sequence < world.transcript.nextSeq

theorem exact_append_replay_is_inert
    (world : World) (document : DocId) (tool : OwnedTool) (record : Segment)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (present : record ∈ world.segments)
    (valid : CanonicalOutput.ToolDelivery.appendReplayValid world.segments
      tool.requestDoc tool.document record = true) :
    appendToolOutput world document record = .ok world := by
  simp [appendToolOutput, found, bound, present, valid]

theorem exact_close_replay_is_inert
    (world : World) (document : DocId) (tool : OwnedTool)
    (authority : CloseAuthority) (record : Segment)
    (coherent : toolLifecycleProjectionCoherent world = true)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (valid : closeReplayValid world tool record = true) :
    closeToolOutput world document authority record = .ok world := by
  simp [closeToolOutput, coherent, found, bound, valid]

theorem exact_publication_replay_ignores_clock_and_cursor
    (world : World) (document : DocId) (tool : OwnedTool)
    (message : MessageEnvelope)
    (coherent : toolProjectionCoherent world = true)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (shape : deliveryShape? message document = some .foregroundResult)
    (valid : publicationReplayValid world tool message = true) :
    publishToolDelivery world document message = .ok world := by
  simp [publishToolDelivery, coherent, found, bound, shape, valid]

theorem exact_background_receipt_replay_ignores_clock_and_cursor
    (world : World) (document : DocId) (tool : OwnedTool)
    (closing : Segment) (message : MessageEnvelope)
    (coherent : toolProjectionCoherent world = true)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (valid : backgroundReceiptReplayValid world tool closing message = true) :
    publishBackgroundReceipt world document closing message = .ok world := by
  simp [publishBackgroundReceipt, coherent, found, bound, valid]

theorem appendToolOutput_maps_constant_lease (world : World) (document : DocId)
    (record : Segment) :
    (appendToolOutput world document record).map (fun post => post.lease) =
      (appendToolOutput world document record).map (fun _ => world.lease) := by
  unfold appendToolOutput
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  dsimp
  split <;> try rfl
  split <;> try rfl

theorem append_preserves_parent_lease (before after : World) (document : DocId)
    (record : Segment) (h : appendToolOutput before document record = .ok after) :
    after.lease = before.lease := by
  have hm := congrArg (Except.map (fun post => post.lease)) h
  rw [appendToolOutput_maps_constant_lease] at hm
  simp only [h] at hm
  change Except.ok before.lease = Except.ok after.lease at hm
  exact (Except.ok.inj hm).symm

theorem appendToolOutput_maps_constant_nextSeq (world : World) (document : DocId)
    (record : Segment) :
    (appendToolOutput world document record).map (fun post => post.transcript.nextSeq) =
      (appendToolOutput world document record).map (fun _ => world.transcript.nextSeq) := by
  unfold appendToolOutput
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  dsimp
  split <;> try rfl

theorem append_preserves_nextSeq (before after : World) (document : DocId)
    (record : Segment) (h : appendToolOutput before document record = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq := by
  have hm := congrArg (Except.map (fun post => post.transcript.nextSeq)) h
  rw [appendToolOutput_maps_constant_nextSeq] at hm
  simp only [h] at hm
  change Except.ok before.transcript.nextSeq = Except.ok after.transcript.nextSeq at hm
  exact (Except.ok.inj hm).symm

theorem closeToolOutput_maps_constant_lease (world : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment) :
    (closeToolOutput world document authority record).map (fun post => post.lease) =
      (closeToolOutput world document authority record).map (fun _ => world.lease) := by
  unfold closeToolOutput
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  dsimp
  split <;> rfl

theorem close_preserves_parent_lease (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    after.lease = before.lease := by
  have hm := congrArg (Except.map (fun post => post.lease)) h
  rw [closeToolOutput_maps_constant_lease] at hm
  simp only [h] at hm
  change Except.ok before.lease = Except.ok after.lease at hm
  exact (Except.ok.inj hm).symm

theorem closeToolOutput_maps_constant_nextSeq (world : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment) :
    (closeToolOutput world document authority record).map
        (fun post => post.transcript.nextSeq) =
      (closeToolOutput world document authority record).map
        (fun _ => world.transcript.nextSeq) := by
  unfold closeToolOutput
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  dsimp [Transcript.TranscriptState.terminalizeToolCall]
  split <;> rfl

theorem close_preserves_nextSeq (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq := by
  have hm := congrArg (Except.map (fun post => post.transcript.nextSeq)) h
  rw [closeToolOutput_maps_constant_nextSeq] at hm
  simp only [h] at hm
  change Except.ok before.transcript.nextSeq = Except.ok after.transcript.nextSeq at hm
  exact (Except.ok.inj hm).symm

set_option maxHeartbeats 1000000 in
theorem publishToolDelivery_maps_constant_lease (world : World) (document : DocId)
    (message : MessageEnvelope) :
    (publishToolDelivery world document message).map (fun post => post.lease) =
      (publishToolDelivery world document message).map (fun _ => world.lease) := by
  unfold publishToolDelivery
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  dsimp
  split <;> try rfl
  split <;> try rfl

theorem publication_preserves_parent_lease (before after : World) (document : DocId)
    (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    after.lease = before.lease := by
  have hm := congrArg (Except.map (fun post => post.lease)) h
  rw [publishToolDelivery_maps_constant_lease] at hm
  simp only [h] at hm
  change Except.ok before.lease = Except.ok after.lease at hm
  exact (Except.ok.inj hm).symm

set_option maxHeartbeats 1000000 in
theorem publication_allocator_replays_or_advances
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq ∨
      after.transcript.nextSeq = before.transcript.nextSeq + 1 := by
  unfold publishToolDelivery at h
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  cases h
  have advance := fresh_delivery_advances_allocator before _ message _ (by assumption)
  exact Or.inr advance

theorem publication_nextSeq_monotone
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  rcases publication_allocator_replays_or_advances before after document message h with
    same | fresh
  · rw [same]
  · rw [fresh]
    exact Nat.le_add_right _ 1

private theorem backgroundNotification_maps_constant_lease
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (world : World) (document : DocId) (message : MessageEnvelope) :
    (publishBackgroundNotificationWith headerValid world document message).map
        (fun post => post.lease) =
      (publishBackgroundNotificationWith headerValid world document message).map
        (fun _ => world.lease) := by
  unfold publishBackgroundNotificationWith
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  dsimp
  split <;> try rfl
  split <;> try rfl

private theorem backgroundNotification_allocator_replays_or_advances
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishBackgroundNotificationWith headerValid before document message = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq ∨
      after.transcript.nextSeq = before.transcript.nextSeq + 1 := by
  unfold publishBackgroundNotificationWith at h
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  split at h <;> try simp_all
  cases h
  have advance := fresh_delivery_advances_allocator before _ message _ (by assumption)
  exact Or.inr advance

theorem wake_notification_preserves_parent_lease
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    after.lease = before.lease := by
  unfold publishWakeNotification at h
  have hm := congrArg (Except.map (fun post => post.lease)) h
  rw [backgroundNotification_maps_constant_lease] at hm
  simp only [h] at hm
  change Except.ok before.lease = Except.ok after.lease at hm
  exact (Except.ok.inj hm).symm

theorem goal_notification_preserves_parent_lease
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    after.lease = before.lease := by
  unfold publishGoalNotification at h
  have hm := congrArg (Except.map (fun post => post.lease)) h
  rw [backgroundNotification_maps_constant_lease] at hm
  simp only [h] at hm
  change Except.ok before.lease = Except.ok after.lease at hm
  exact (Except.ok.inj hm).symm

theorem wake_notification_allocator_replays_or_advances
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq ∨
      after.transcript.nextSeq = before.transcript.nextSeq + 1 := by
  exact backgroundNotification_allocator_replays_or_advances
    (fun world tool message => wakeNotificationHeaderValid world tool binding message)
    before after document message h

theorem goal_notification_allocator_replays_or_advances
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq ∨
      after.transcript.nextSeq = before.transcript.nextSeq + 1 := by
  exact backgroundNotification_allocator_replays_or_advances
    (fun world tool message => goalNotificationHeaderValid world tool binding message)
    before after document message h

theorem wake_notification_nextSeq_monotone
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  rcases wake_notification_allocator_replays_or_advances
      before after document binding message h with same | fresh
  · rw [same]
  · rw [fresh]
    exact Nat.le_add_right _ 1

theorem goal_notification_nextSeq_monotone
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  rcases goal_notification_allocator_replays_or_advances
      before after document binding message h with same | fresh
  · rw [same]
  · rw [fresh]
    exact Nat.le_add_right _ 1

set_option maxHeartbeats 1000000 in
theorem background_receipt_maps_constant_lease
    (world : World) (document : DocId) (closing : Segment) (message : MessageEnvelope) :
    (publishBackgroundReceipt world document closing message).map (fun post => post.lease) =
      (publishBackgroundReceipt world document closing message).map (fun _ => world.lease) := by
  unfold publishBackgroundReceipt
  -- projection, lookup, binding, replay, and fresh record admission
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  dsimp
  -- header, clock, lifecycle, sequence, cursor, and transcript row
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  -- unique result key and post-projection integrity
  split <;> try rfl
  split <;> try rfl

theorem background_receipt_preserves_parent_lease
    (before after : World) (document : DocId) (closing : Segment) (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    after.lease = before.lease := by
  have hm := congrArg (Except.map (fun post => post.lease)) h
  rw [background_receipt_maps_constant_lease] at hm
  simp only [h] at hm
  change Except.ok before.lease = Except.ok after.lease at hm
  exact (Except.ok.inj hm).symm

set_option maxHeartbeats 1000000 in
theorem background_receipt_allocator_replays_or_advances
    (before after : World) (document : DocId) (closing : Segment) (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq ∨
      after.transcript.nextSeq = before.transcript.nextSeq + 1 := by
  unfold publishBackgroundReceipt at h
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at h <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  rw [← h]
  exact Or.inr rfl

theorem background_receipt_nextSeq_monotone
    (before after : World) (document : DocId) (closing : Segment) (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  rcases background_receipt_allocator_replays_or_advances before after document closing message h with
    same | fresh
  · rw [same]
  · rw [fresh]
    exact Nat.le_add_right _ 1

end CanonicalOutput.Execution.ToolDelivery
