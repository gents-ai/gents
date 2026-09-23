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

/-- The complete write authority of tool delivery. Identity, request control,
the parent lease, routing, compaction authority, delegation, and terminal
selection remain read-only inputs from `World`. -/
private structure ToolWrite where
  segments : List Segment
  messages : List MessageEnvelope
  transcript : Transcript.TranscriptState
  toolContexts : List OwnedTool

private def ToolWrite.current (world : World) : ToolWrite :=
  { segments := world.segments
  , messages := world.messages
  , transcript := world.transcript
  , toolContexts := world.toolContexts }

private def ToolWrite.apply (world : World) (write : ToolWrite) : World :=
  { world with
    segments := write.segments
    messages := write.messages
    transcript := write.transcript
    toolContexts := write.toolContexts }

@[simp] private theorem ToolWrite.apply_current (world : World) :
    ToolWrite.apply world (ToolWrite.current world) = world := by
  cases world
  rfl

private def ToolWrite.lift (world : World) (result : Except Error ToolWrite) : Except Error World :=
  result.map (ToolWrite.apply world)

private theorem ToolWrite.lift_success {world after : World} {result : Except Error ToolWrite}
    (h : ToolWrite.lift world result = .ok after) :
    ∃ write, result = .ok write ∧ after = ToolWrite.apply world write := by
  cases result with
  | error error => simp [ToolWrite.lift, Except.map] at h
  | ok write =>
      have heq : ToolWrite.apply world write = after := by
        simpa [ToolWrite.lift, Except.map] using h
      exact ⟨write, rfl, heq.symm⟩

private theorem ToolWrite.lift_preserves_lease {world after : World}
    {result : Except Error ToolWrite} (h : ToolWrite.lift world result = .ok after) :
    after.lease = world.lease := by
  obtain ⟨write, _, rfl⟩ := ToolWrite.lift_success h
  rfl

/-- All lifted tool writes share this identity frame, independently of their
branch logic. The private write payload is inferred from a public operation. -/
theorem tool_write_preserves_request_identity {before after : World}
    {result : Except Error ToolWrite} (h : ToolWrite.lift before result = .ok after) :
    after.requestId = before.requestId ∧ after.sessionId = before.sessionId := by
  obtain ⟨write, _, rfl⟩ := ToolWrite.lift_success h
  exact ⟨rfl, rfl⟩

theorem tool_write_preserves_composed_control {before after : World}
    {result : Except Error ToolWrite} (h : ToolWrite.lift before result = .ok after) :
    after.queue = before.queue ∧ after.claimed = before.claimed ∧
      after.retry = before.retry := by
  obtain ⟨write, _, rfl⟩ := ToolWrite.lift_success h
  exact ⟨rfl, rfl, rfl⟩

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
      !decide (isTerminal terminal.state) then none
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
  if decide (isTerminal terminal.state) then some terminal else none

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
private def appendToolOutputWrite (world : World) (document : DocId)
    (record : Segment) : Except Error ToolWrite :=
  match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if record ∈ world.segments &&
          CanonicalOutput.ToolDelivery.appendReplayValid world.segments
            tool.requestDoc tool.document record then .ok (ToolWrite.current world)
      else match updateClock world tool with
      | none => .error .clock
      | some observed =>
          if observed.context.state != .running || observed.context.deadlineExceeded then
            .error .lifecycle
          else match CanonicalOutput.ToolDelivery.appendRecords world.segments
              tool.requestDoc tool.document world.lease.now record with
          | .error error => .error (.source error)
          | .ok segments =>
              .ok { ToolWrite.current world with
                segments := segments
                toolContexts := replaceOwnedTool world.toolContexts document observed }

def appendToolOutput (world : World) (document : DocId)
    (record : Segment) : Except Error World :=
  ToolWrite.lift world (appendToolOutputWrite world document record)

def closeReplayValid (world : World) (tool : OwnedTool) (record : Segment) : Bool :=
  decide (isTerminal tool.context.state) &&
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
private def closeToolOutputWrite (world : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment) : Except Error ToolWrite :=
  if !toolLifecycleProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if closeReplayValid world tool record then .ok (ToolWrite.current world)
      else match terminalContext? world tool authority with
      | none => .error .lifecycle
      | some context =>
          match CanonicalOutput.ToolDelivery.closeRecords world.segments
              tool.requestDoc tool.document world.lease.now record with
          | .error error => .error (.source error)
          | .ok segments =>
              let updated := clearReconcileIntent tool context
              let write := { ToolWrite.current world with
                segments := segments
                toolContexts := replaceOwnedTool world.toolContexts document updated
                transcript := world.transcript.terminalizeToolCall document context.state }
              let post := ToolWrite.apply world write
              if !toolLifecycleProjectionCoherent post then .error .ownership else .ok write

def closeToolOutput (world : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment) : Except Error World :=
  ToolWrite.lift world (closeToolOutputWrite world document authority record)

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
    decide (isTerminal tool.context.state) &&
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
    decide (isTerminal tool.context.state) &&
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
private def publishBackgroundReceiptWrite (world : World) (document : DocId)
    (closing : Segment) (message : MessageEnvelope) : Except Error ToolWrite :=
  if !toolProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if backgroundReceiptReplayValid world tool closing message then
        .ok (ToolWrite.current world)
      else if !backgroundReceiptRecordValid world tool closing ||
          world.messages.any (matchingMessageIdentity message) then .error .publication
      else
        let staged := ToolWrite.apply world
          { ToolWrite.current world with segments := world.segments ++ [closing] }
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
              let write := { ToolWrite.current world with
                segments := staged.segments
                messages := world.messages ++ [message]
                transcript := transcript
                toolContexts := replaceOwnedTool world.toolContexts document updated }
              let post := ToolWrite.apply world write
              if !toolProjectionCoherent post then .error .ownership else .ok write

def publishBackgroundReceipt (world : World) (document : DocId)
    (closing : Segment) (message : MessageEnvelope) : Except Error World :=
  ToolWrite.lift world (publishBackgroundReceiptWrite world document closing message)

/-- Fresh publication allocates exactly `transcript.nextSeq` and derives its
result key from session, physical tool document, and immutable header identity.
Exact replay allocates nothing and is allowed even after compaction advanced. -/
private def publishToolDeliveryWrite (world : World) (document : DocId)
    (message : MessageEnvelope) : Except Error ToolWrite :=
  if !toolProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if deliveryShape? message document != some .foregroundResult then
        .error .publication
      else if publicationReplayValid world tool message then .ok (ToolWrite.current world)
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
              let write := { ToolWrite.current world with
                messages := world.messages ++ [message]
                transcript := transcript
                toolContexts := replaceOwnedTool world.toolContexts document updated }
              let post := ToolWrite.apply world write
              if !toolProjectionCoherent post then .error .ownership else .ok write

def publishToolDelivery (world : World) (document : DocId)
    (message : MessageEnvelope) : Except Error World :=
  ToolWrite.lift world (publishToolDeliveryWrite world document message)

/-- Native completion closes the physical tool source, terminalizes its tool
row, and publishes the paired ToolResult in one storage transaction. Neither
the closed-but-undelivered intermediate world nor a result-only world is
externally observable. -/
def completeAndDeliver (world : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (message : MessageEnvelope) : Except Error World :=
  match closeToolOutput world document authority record with
  | .error error => .error error
  | .ok closed => publishToolDelivery closed document message

theorem completeAndDeliver_success (world after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment) (message : MessageEnvelope)
    (h : completeAndDeliver world document authority record message = .ok after) :
    ∃ closed, closeToolOutput world document authority record = .ok closed ∧
      publishToolDelivery closed document message = .ok after := by
  cases hc : closeToolOutput world document authority record with
  | error error => simp [completeAndDeliver, hc] at h
  | ok closed =>
      simp only [completeAndDeliver, hc] at h
      exact ⟨closed, rfl, h⟩

private def publishBackgroundNotificationWriteWith
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (world : World) (document : DocId) (message : MessageEnvelope) : Except Error ToolWrite :=
  if !toolProjectionCoherent world then .error .ownership
  else match ownedToolByDocument? world document with
  | none => .error .missingTool
  | some tool =>
      if !bindingValid world tool then .error .ownership
      else if notificationReplayValid headerValid world tool message then
        .ok (ToolWrite.current world)
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
              let write := { ToolWrite.current world with
                messages := world.messages ++ [message]
                transcript := transcript
                toolContexts := replaceOwnedTool world.toolContexts document updated }
              let post := ToolWrite.apply world write
              if !toolProjectionCoherent post then .error .ownership else .ok write

private def publishBackgroundNotificationWith
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (world : World) (document : DocId) (message : MessageEnvelope) : Except Error World :=
  ToolWrite.lift world
    (publishBackgroundNotificationWriteWith headerValid world document message)

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

theorem exact_append_replay_is_inert
    (world : World) (document : DocId) (tool : OwnedTool) (record : Segment)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (present : record ∈ world.segments)
    (valid : CanonicalOutput.ToolDelivery.appendReplayValid world.segments
      tool.requestDoc tool.document record = true) :
    appendToolOutput world document record = .ok world := by
  simp [appendToolOutput, appendToolOutputWrite, ToolWrite.lift, Except.map,
    found, bound, present, valid]

theorem exact_close_replay_is_inert
    (world : World) (document : DocId) (tool : OwnedTool)
    (authority : CloseAuthority) (record : Segment)
    (coherent : toolLifecycleProjectionCoherent world = true)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (valid : closeReplayValid world tool record = true) :
    closeToolOutput world document authority record = .ok world := by
  simp [closeToolOutput, closeToolOutputWrite, ToolWrite.lift, Except.map,
    coherent, found, bound, valid]

theorem exact_publication_replay_ignores_clock_and_cursor
    (world : World) (document : DocId) (tool : OwnedTool)
    (message : MessageEnvelope)
    (coherent : toolProjectionCoherent world = true)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (shape : deliveryShape? message document = some .foregroundResult)
    (valid : publicationReplayValid world tool message = true) :
    publishToolDelivery world document message = .ok world := by
  simp [publishToolDelivery, publishToolDeliveryWrite, ToolWrite.lift, Except.map,
    coherent, found, bound, shape, valid]

theorem exact_background_receipt_replay_ignores_clock_and_cursor
    (world : World) (document : DocId) (tool : OwnedTool)
    (closing : Segment) (message : MessageEnvelope)
    (coherent : toolProjectionCoherent world = true)
    (found : ownedToolByDocument? world document = some tool)
    (bound : bindingValid world tool = true)
    (valid : backgroundReceiptReplayValid world tool closing message = true) :
    publishBackgroundReceipt world document closing message = .ok world := by
  simp [publishBackgroundReceipt, publishBackgroundReceiptWrite, ToolWrite.lift, Except.map,
    coherent, found, bound, valid]

theorem append_preserves_parent_lease (before after : World) (document : DocId)
    (record : Segment) (h : appendToolOutput before document record = .ok after) :
    after.lease = before.lease := by
  exact ToolWrite.lift_preserves_lease h

/-- The lifecycle-relevant effect of a successful append. This exposes the
single owned-tool replacement while keeping the write payload private. -/
theorem append_success_lifecycle_effect (before after : World) (document : DocId)
    (record : Segment) (h : appendToolOutput before document record = .ok after) :
    after = before ∨ ∃ tool observed segments,
      ownedToolByDocument? before document = some tool ∧
      updateClock before tool = some observed ∧
      CanonicalOutput.ToolDelivery.appendRecords before.segments
        tool.requestDoc tool.document before.lease.now record = .ok segments ∧
      after = { before with
        segments := segments
        toolContexts := replaceOwnedTool before.toolContexts document observed } := by
  unfold appendToolOutput ToolWrite.lift appendToolOutputWrite at h
  split at h <;> try contradiction
  rename_i tool found
  split at h <;> try contradiction
  split at h
  · left
    simpa [Except.map, ToolWrite.apply_current] using h.symm
  · split at h <;> try contradiction
    rename_i observed clocked
    split at h <;> try contradiction
    split at h <;> try contradiction
    rename_i segments appended
    right
    refine ⟨tool, observed, segments, found, clocked, appended, ?_⟩
    simpa [Except.map, ToolWrite.apply, ToolWrite.current] using h.symm

theorem append_preserves_publications (before after : World) (document : DocId)
    (record : Segment) (h : appendToolOutput before document record = .ok after) :
    after.sessionId = before.sessionId ∧ after.messages = before.messages ∧
      after.transcript.nextSeq = before.transcript.nextSeq := by
  rcases append_success_lifecycle_effect before after document record h with same | effect
  · subst after
    exact ⟨rfl, rfl, rfl⟩
  · rcases effect with ⟨tool, observed, segments, found, clocked, appended, rfl⟩
    exact ⟨rfl, rfl, rfl⟩

theorem append_success_segment_effect (before after : World) (document : DocId)
    (record : Segment) (h : appendToolOutput before document record = .ok after) :
    after.segments = before.segments ∨
      (after.segments = before.segments ++ [record] ∧ record.close = none ∧
        closures before.segments record.coordinate = [] ∧
        CanonicalOutput.ToolDelivery.identityAvailable before.segments record = true) := by
  rcases append_success_lifecycle_effect before after document record h with same | effect
  · subst after
    exact Or.inl rfl
  · rcases effect with ⟨tool, observed, segments, found, clocked, appended, rfl⟩
    rcases CanonicalOutput.ToolDelivery.appendRecords_success_effect
      before.segments segments tool.requestDoc tool.document before.lease.now record appended with
      replay | fresh
    · exact Or.inl replay
    · exact Or.inr fresh

theorem append_preserves_nextSeq (before after : World) (document : DocId)
    (record : Segment) (h : appendToolOutput before document record = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq :=
  (append_preserves_publications before after document record h).2.2

theorem close_preserves_parent_lease (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    after.lease = before.lease := by
  exact ToolWrite.lift_preserves_lease h

theorem close_success_effect (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    (after.sessionId = before.sessionId ∧ after.messages = before.messages ∧
      after.transcript.nextSeq = before.transcript.nextSeq) ∧
      toolLifecycleProjectionCoherent after = true := by
  unfold closeToolOutput ToolWrite.lift closeToolOutputWrite at h
  split at h <;> try contradiction
  rename_i preCoherent
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h
  · have same : before = after := by
      simpa [Except.map, ToolWrite.apply, ToolWrite.current] using h
    rw [← same]
    exact ⟨⟨rfl, rfl, rfl⟩, by simpa using preCoherent⟩
  · split at h <;> try contradiction
    split at h <;> try contradiction
    dsimp at h
    split at h
    · contradiction
    · rename_i postCoherent
      simp [Except.map] at h
      subst after
      exact ⟨⟨rfl, rfl, rfl⟩, by simpa using postCoherent⟩

theorem close_preserves_publications (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    after.sessionId = before.sessionId ∧ after.messages = before.messages ∧
      after.transcript.nextSeq = before.transcript.nextSeq := by
  exact (close_success_effect before after document authority record h).1

theorem close_success_write_effect (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    after = before ∨ ∃ tool context segments,
      ownedToolByDocument? before document = some tool ∧
      terminalContext? before tool authority = some context ∧
      CanonicalOutput.ToolDelivery.closeRecords before.segments tool.requestDoc tool.document
        before.lease.now record = .ok segments ∧
      after = { before with
        segments := segments
        toolContexts := replaceOwnedTool before.toolContexts document (clearReconcileIntent tool context)
        transcript := before.transcript.terminalizeToolCall document context.state } := by
  unfold closeToolOutput ToolWrite.lift closeToolOutputWrite at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  rename_i tool found
  split at h <;> try contradiction
  split at h
  · left; simpa [Except.map, ToolWrite.apply_current] using h.symm
  · split at h <;> try contradiction
    rename_i context terminal
    split at h <;> try contradiction
    rename_i segments closed
    dsimp at h
    split at h <;> try contradiction
    simp [Except.map] at h
    subst after
    exact Or.inr ⟨tool, context, segments, found, terminal, closed, rfl⟩

theorem closeToolOutput_success_segment_effect (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    after.segments = before.segments ∨
      ∃ request sourceDoc, after.segments = before.segments ++ [record] ∧
        closures before.segments (CanonicalOutput.ToolDelivery.coordinate request sourceDoc) = [] ∧
        CanonicalOutput.ToolDelivery.identityAvailable before.segments record = true ∧
        CanonicalOutput.ToolDelivery.ownedRecord before.segments request sourceDoc
          before.lease.now record = true := by
  rcases close_success_write_effect before after document authority record h with same | effect
  · exact Or.inl (congrArg World.segments same)
  · obtain ⟨tool, context, segments, _, _, closed, rfl⟩ := effect
    rcases CanonicalOutput.ToolDelivery.closeRecords_success_effect
      before.segments segments tool.requestDoc tool.document before.lease.now record closed with
      replay | fresh
    · exact Or.inl replay
    · exact Or.inr ⟨tool.requestDoc, tool.document, fresh⟩

theorem close_preserves_nextSeq (before after : World) (document : DocId)
    (authority : CloseAuthority) (record : Segment)
    (h : closeToolOutput before document authority record = .ok after) :
    after.transcript.nextSeq = before.transcript.nextSeq :=
  (close_preserves_publications before after document authority record h).2.2

theorem publication_preserves_parent_lease (before after : World) (document : DocId)
    (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    after.lease = before.lease := by
  exact ToolWrite.lift_preserves_lease h

/-- Successful publication is either exact replay or a single header allocated
at the old cursor. This one effect accounts for both the message collection and
its allocator, rather than proving unrelated frame facts about each. -/
def PublicationEffect (before after : World) (message : MessageEnvelope) : Prop :=
  after.sessionId = before.sessionId ∧
    ((after.messages = before.messages ∧
        after.transcript.nextSeq = before.transcript.nextSeq) ∨
      (after.messages = before.messages ++ [message] ∧
        message.sequence = before.transcript.nextSeq ∧
        after.transcript.nextSeq = before.transcript.nextSeq + 1))

theorem PublicationEffect.nextSeq_monotone {before after : World} {message : MessageEnvelope}
    (effect : PublicationEffect before after message) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  rcases effect.2 with replay | fresh
  · rw [replay.2]
  · rw [fresh.2.2]
    exact Nat.le_add_right _ 1

set_option maxHeartbeats 1000000 in
theorem publication_effect
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    PublicationEffect before after message := by
  unfold publishToolDelivery at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  clear h
  unfold publishToolDeliveryWrite at hwrite
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  · rw [← hwrite, ToolWrite.apply_current]
    exact ⟨rfl, Or.inl ⟨rfl, rfl⟩⟩
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  cases hwrite
  have advance := fresh_delivery_advances_allocator before _ message _ (by assumption)
  exact ⟨rfl, Or.inr ⟨rfl, by simp_all, advance⟩⟩

theorem publication_nextSeq_monotone
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq :=
  (publication_effect before after document message h).nextSeq_monotone

set_option maxHeartbeats 1000000 in
theorem publishToolDelivery_preserves_segments
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    after.segments = before.segments := by
  unfold publishToolDelivery at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  unfold publishToolDeliveryWrite at hwrite
  repeat' first
    | contradiction
    | (solve | cases hwrite; rfl)
    | (solve | simp_all [ToolWrite.apply_current, ToolWrite.apply, ToolWrite.current])
    | split at hwrite
  dsimp at hwrite
  split at hwrite <;> try contradiction
  simp at hwrite
  rw [← hwrite]
  rfl

private theorem backgroundNotification_effect
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishBackgroundNotificationWith headerValid before document message = .ok after) :
    PublicationEffect before after message := by
  unfold publishBackgroundNotificationWith at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  clear h
  unfold publishBackgroundNotificationWriteWith at hwrite
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  · rw [← hwrite, ToolWrite.apply_current]
    exact ⟨rfl, Or.inl ⟨rfl, rfl⟩⟩
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  cases hwrite
  have advance := fresh_delivery_advances_allocator before _ message _ (by assumption)
  exact ⟨rfl, Or.inr ⟨rfl, by simp_all, advance⟩⟩

set_option maxHeartbeats 1000000 in
private theorem publishBackgroundNotification_preserves_segments
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishBackgroundNotificationWith headerValid before document message = .ok after) :
    after.segments = before.segments := by
  unfold publishBackgroundNotificationWith at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  unfold publishBackgroundNotificationWriteWith at hwrite
  repeat' first
    | contradiction
    | (solve | cases hwrite; rfl)
    | (solve | simp_all [ToolWrite.apply_current, ToolWrite.apply, ToolWrite.current])
    | split at hwrite
  dsimp at hwrite
  split at hwrite <;> try contradiction
  simp at hwrite
  rw [← hwrite]
  rfl

theorem wake_notification_preserves_parent_lease
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    after.lease = before.lease := by
  exact ToolWrite.lift_preserves_lease h

/-- Wake notification publication has only `ToolWrite` authority; composed
request identity, queue/claim ownership, and retry policy remain framed. -/
theorem wake_notification_preserves_composed_control
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    after.requestId = before.requestId ∧ after.sessionId = before.sessionId ∧
      after.queue = before.queue ∧ after.claimed = before.claimed ∧
      after.retry = before.retry := by
  unfold publishWakeNotification publishBackgroundNotificationWith at h
  obtain ⟨write, _, rfl⟩ := ToolWrite.lift_success h
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

theorem goal_notification_preserves_parent_lease
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    after.lease = before.lease := by
  exact ToolWrite.lift_preserves_lease h

theorem wake_notification_preserves_segments
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    after.segments = before.segments :=
  publishBackgroundNotification_preserves_segments
    (fun world tool candidate => wakeNotificationHeaderValid world tool binding candidate)
    before after document message h

theorem goal_notification_preserves_segments
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    after.segments = before.segments :=
  publishBackgroundNotification_preserves_segments
    (fun world tool candidate => goalNotificationHeaderValid world tool binding candidate)
    before after document message h

theorem wake_notification_effect
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    PublicationEffect before after message :=
  backgroundNotification_effect
    (fun world tool message => wakeNotificationHeaderValid world tool binding message)
    before after document message h

theorem goal_notification_effect
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    PublicationEffect before after message :=
  backgroundNotification_effect
    (fun world tool message => goalNotificationHeaderValid world tool binding message)
    before after document message h

theorem wake_notification_nextSeq_monotone
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq :=
  (wake_notification_effect before after document binding message h).nextSeq_monotone

theorem goal_notification_nextSeq_monotone
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq :=
  (goal_notification_effect before after document binding message h).nextSeq_monotone

theorem background_receipt_preserves_parent_lease
    (before after : World) (document : DocId) (closing : Segment) (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    after.lease = before.lease := by
  exact ToolWrite.lift_preserves_lease h

set_option maxHeartbeats 1000000 in
theorem background_receipt_effect
    (before after : World) (document : DocId) (closing : Segment) (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    PublicationEffect before after message := by
  unfold publishBackgroundReceipt at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  clear h
  unfold publishBackgroundReceiptWrite at hwrite
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  · rw [← hwrite, ToolWrite.apply_current]
    exact ⟨rfl, Or.inl ⟨rfl, rfl⟩⟩
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  rw [← hwrite]
  exact ⟨rfl, Or.inr ⟨rfl, by simp_all, rfl⟩⟩

theorem background_receipt_nextSeq_monotone
    (before after : World) (document : DocId) (closing : Segment) (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq :=
  (background_receipt_effect before after document closing message h).nextSeq_monotone

set_option maxHeartbeats 1000000 in
theorem background_receipt_segment_effect
    (before after : World) (document : DocId) (closing : Segment)
    (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    after.segments = before.segments ∨
      (after.segments = before.segments ++ [closing] ∧
        freshSegmentIdentity before closing = true ∧
        sourceOpen before closing.coordinate = true) := by
  unfold publishBackgroundReceipt at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  clear h
  unfold publishBackgroundReceiptWrite at hwrite
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite
  · left
    simp at hwrite
    subst write
    rfl
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    backgroundReceiptRecordValid, Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [backgroundReceiptRecordValid,
    Transcript.TranscriptState.publishToolResult]
  rw [← hwrite]
  right
  simp [ToolWrite.apply, ToolWrite.current]

set_option maxHeartbeats 1000000 in
/-- Every successful foreground delivery leaves the complete tool projection
coherent.  This exposes the postcondition already enforced by the publication
transaction, including its exact-replay branch. -/
theorem publishToolDelivery_success_toolProjectionCoherent
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishToolDelivery before document message = .ok after) :
    toolProjectionCoherent after = true := by
  unfold publishToolDelivery at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  clear h
  unfold publishToolDeliveryWrite at hwrite
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  rename_i preCoherent
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  · rw [← hwrite, ToolWrite.apply_current]
    simpa using preCoherent
  repeat' first | contradiction | (solve | simp_all) | split at hwrite

set_option maxHeartbeats 1000000 in
/-- The atomic background receipt path checks the same complete projection
after installing its closure, message, transcript result, and tool state. -/
theorem publishBackgroundReceipt_success_toolProjectionCoherent
    (before after : World) (document : DocId) (closing : Segment)
    (message : MessageEnvelope)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    toolProjectionCoherent after = true := by
  unfold publishBackgroundReceipt at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  clear h
  unfold publishBackgroundReceiptWrite at hwrite
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  rename_i preCoherent
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [ToolWrite.apply_current,
    Transcript.TranscriptState.publishToolResult]
  · rw [← hwrite, ToolWrite.apply_current]
    simpa using preCoherent
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]
  split at hwrite <;> try simp_all [Transcript.TranscriptState.publishToolResult]

private theorem publishBackgroundNotification_success_toolProjectionCoherent
    (headerValid : World → OwnedTool → MessageEnvelope → Bool)
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (h : publishBackgroundNotificationWith headerValid before document message = .ok after) :
    toolProjectionCoherent after = true := by
  unfold publishBackgroundNotificationWith at h
  obtain ⟨write, hwrite, rfl⟩ := ToolWrite.lift_success h
  clear h
  unfold publishBackgroundNotificationWriteWith at hwrite
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  rename_i preCoherent
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  split at hwrite <;> try simp_all [ToolWrite.apply_current]
  · rw [← hwrite, ToolWrite.apply_current]
    simpa using preCoherent
  repeat' first | contradiction | (solve | simp_all) | split at hwrite

theorem publishWakeNotification_success_toolProjectionCoherent
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope)
    (h : publishWakeNotification before document binding message = .ok after) :
    toolProjectionCoherent after = true :=
  publishBackgroundNotification_success_toolProjectionCoherent
    (fun world tool candidate => wakeNotificationHeaderValid world tool binding candidate)
    before after document message h

theorem publishGoalNotification_success_toolProjectionCoherent
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope)
    (h : publishGoalNotification before document binding message = .ok after) :
    toolProjectionCoherent after = true :=
  publishBackgroundNotification_success_toolProjectionCoherent
    (fun world tool candidate => goalNotificationHeaderValid world tool binding candidate)
    before after document message h

end CanonicalOutput.Execution.ToolDelivery
