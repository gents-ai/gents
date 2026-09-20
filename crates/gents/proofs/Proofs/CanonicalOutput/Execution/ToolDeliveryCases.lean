import Proofs.CanonicalOutput.Execution.ToolDelivery
import Proofs.CanonicalOutput.Execution.Examples

namespace CanonicalOutput.Execution.ToolDelivery.Cases

open CanonicalOutput.Execution.Examples

def toolOutputClose : Segment :=
  { id := 700, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .toolOutput }⟩], [79]⟩
    close := some (.closed .complete 1 [1]), createdAt := 5 }

def toolDeliveryMessage (sequence : Nat := 1) : MessageEnvelope :=
  { header :=
      { id := 701, session := 1, request := some 10, origin := none
        refs := [⟨700, 0⟩], outcome := .complete, role := .user
        publication := .toolDelivery 600 }
    key := "tool-notification-600", sequence := sequence, nativeId := none
    blocks := [.text ⟨⟨700, 0⟩, .composed [.literal [91], .range 0 1, .literal [93]]⟩]
    createdAt := 5 }

def wakeEntry : SessionQueue.QueueEntry :=
  { requestId := 901, createdAt := 5, source := .backgroundCompletion
    policy := .coalesce, queueKey := some 1, queuedAfter := some 10 }

def wakeNotificationMessage (sequence : Nat := 2) : MessageEnvelope :=
  { toolDeliveryMessage sequence with
    header := { (toolDeliveryMessage sequence).header with request := some 800 } }

def wakeBinding (message : MessageEnvelope := wakeNotificationMessage) : WakeDocumentBinding :=
  { entry := wakeEntry, agent := 1, session := 1
    notificationMessageId := message.header.id
    notificationSequence := message.sequence
    wakeDocument := 800, authenticated := true }

def goalBinding : GoalNotificationBinding :=
  { goalDocument := 900, parentRequestDocument := 10, agent := 1, session := 1
    status := .active, authenticated := true }

def foregroundResultMessage (sequence : Nat := 1) : MessageEnvelope :=
  { toolDeliveryMessage sequence with
    key := "tool-result-600"
    blocks := [.toolResult 600 "native-call" none
      [.text ⟨⟨700, 0⟩, .composed [.literal [91], .range 0 1, .literal [93]]⟩]] }

def backgroundReceiptClose : Segment :=
  { toolOutputClose with id := 702, coordinate := ⟨10, .authored 99⟩ }

def backgroundReceiptMessage : MessageEnvelope :=
  { foregroundResultMessage 1 with
    header := { (foregroundResultMessage 1).header with id := 703, refs := [⟨702, 0⟩] }
    key := "background-receipt-600"
    blocks := [.toolResult 600 "native-call" none
      [.text ⟨⟨702, 0⟩, .composed [.literal [91], .range 0 1, .literal [93]]⟩]] }

def bridgeAdmission : ToolAdmission :=
  ⟨600, { foregroundToolContext with childRequestId := some 42 }⟩

def requestContext (state : RequestState) (requestId : RequestId) : RequestContext :=
  { state := state
    origin := .interactive
    backend := ⟨"test"⟩
    admission := if isTerminal state then .released else .executing
    deadline := 30
    claimTime := 0
    currentTime := 5
    retryCount := 0
    maxRetries := 1
    messageSeq := 0
    persistence := .committed
    causedByParentRequestId := if requestId == 42 then some 10 else none
    causedByParentToolCallId := if requestId == 42 then some 600 else none }

def composed (requestId : RequestId) (state : RequestState)
    (tools : List ToolExecution.ToolCallContext := []) : ComposedState :=
  { requestId := requestId
  , process := .ready
  , request := requestContext state requestId
  , call :=
      { callId := requestId
      , requestId := requestId
      , backend := ⟨"test"⟩
      , state := .completed }
  , tools := tools }

def completedBridge (context : ToolExecution.ToolCallContext) : Subagent.BridgedState :=
  { parent := composed 10 .completed [context]
  , child := composed 42 .completed
  , bridgeCallId := context.callId }

/-- A linked background child publishes its immediate native result while the
physical call is running, the parent can then finish, and verified child
terminal state closes the tool source before the ordinary late notification. -/
def bridgeReceiptThenNotification : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage []
    [bridgeAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let receipted ← (ToolDelivery.publishBackgroundReceipt backgrounded 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  let terminal ← (terminalize receipted 7 .completed (.message 501)).toOption
  let running ← ownedToolByDocument? terminal 600
  let closed ← (ToolDelivery.closeToolOutput terminal 600
    (.bridge (completedBridge running.context) .bridge_complete) toolOutputClose).toOption
  let delivered ← (ToolDelivery.publishWakeNotification closed 600
    (wakeBinding (wakeNotificationMessage 2)) (wakeNotificationMessage 2)).toOption
  let receiptReplay ← (ToolDelivery.publishBackgroundReceipt delivered 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  let notificationReplay ← (ToolDelivery.publishWakeNotification receiptReplay 600
    (wakeBinding (wakeNotificationMessage 2)) (wakeNotificationMessage 2)).toOption
  let finalTool ← ownedToolByDocument? delivered 600
  let row ← transcriptToolByDocument? delivered 600
  pure (receiptReplay == delivered && notificationReplay == delivered &&
    receipted.transcript.nextSeq == 2 &&
    terminal.lease.request == .completed && finalTool.context.state == .completed &&
    row.state == .completed && row.resultKey.isSome &&
    delivered.transcript.nextSeq == 3 &&
    delivered.messages.contains backgroundReceiptMessage &&
    delivered.messages.contains (wakeNotificationMessage 2))

theorem linked_background_receipt_bridge_and_notification_trace :
    bridgeReceiptThenNotification = some true := by native_decide

/-- An immediate background receipt remains the one native result if the
owner later waits in the foreground.  Completion is blocked while the bridge
is physically running; verified bridge close changes lifecycle only and does
not allocate or publish another result. -/
def receiptThenForegroundClose : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage []
    [bridgeAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let receipted ← (ToolDelivery.publishBackgroundReceipt backgrounded 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  let foregrounded ← (changeToolControl receipted 7 600 .foreground).toOption
  let running ← ownedToolByDocument? foregrounded 600
  let completionBlocked :=
    match terminalize foregrounded 7 .completed (.message 501) with
    | .error _ => true
    | .ok _ => false
  let closed ← (ToolDelivery.closeToolOutput foregrounded 600
    (.bridge (completedBridge running.context) .bridge_complete) toolOutputClose).toOption
  let terminal ← (terminalize closed 7 .completed (.message 501)).toOption
  let finalTool ← ownedToolByDocument? terminal 600
  let row ← transcriptToolByDocument? terminal 600
  pure (completionBlocked && finalTool.context.state == .completed &&
    row.state == .completed && row.resultKey.isSome &&
    terminal.transcript.nextSeq == 2 && terminal.messages.length == 2)

theorem receipt_can_be_followed_by_foreground_wait_and_lifecycle_only_close :
    receiptThenForegroundClose = some true := by native_decide

def authoredReceiptSourcePolicy : Bool :=
  let staged := [backgroundReceiptClose]
  let wrongWriter := { backgroundReceiptClose with writer := .tool 601 }
  let wrongRequest := { backgroundReceiptMessage with header :=
    { backgroundReceiptMessage.header with request := some 11 } }
  let forked := { backgroundReceiptMessage with header :=
    { backgroundReceiptMessage.header with
      request := none
      origin := some 703
      publication := .fork 703 } }
  (match reconstructMessage staged [] backgroundReceiptMessage with
    | .ok _ => true | .error _ => false) &&
  (match reconstructMessage [wrongWriter] [] backgroundReceiptMessage with
    | .error _ => true | .ok _ => false) &&
  (match reconstructMessage staged [] wrongRequest with
    | .error _ => true | .ok _ => false) &&
  (match reconstructMessage staged [] forked with
    | .ok _ => true | .error _ => false)

theorem authored_receipt_requires_exact_writer_and_request_but_fork_retains_ref :
    authoredReceiptSourcePolicy = true := by native_decide

def closeAndPublishState (action : ToolExecution.ToolCallContext.Action)
    (expected : ToolExecution.ToolCallState) (now : Time := 5) : Bool :=
  match acceptAndPublish (world 5) 7 providerTurn providerMessage [] [foregroundAdmission] with
  | .error _ => false
  | .ok accepted => match dispatch accepted 7 permit with
    | .error _ => false
    | .ok dispatched =>
      let timed := { dispatched with lease := { dispatched.lease with now := now } }
      let closing := { toolOutputClose with createdAt := now }
      let message := { foregroundResultMessage 1 with createdAt := now }
      match ToolDelivery.closeToolOutput timed 600 (.native action) closing with
      | .error _ => false
      | .ok closed => match ToolDelivery.publishToolDelivery closed 600 message with
        | .error _ => false
        | .ok delivered =>
          match ownedToolByDocument? delivered 600, transcriptToolByDocument? delivered 600 with
          | some tool, some row =>
              tool.context.state == expected && row.state == expected && row.resultKey.isSome
          | _, _ => false

theorem failed_delivery_preserves_failed_lifecycle :
    closeAndPublishState (.fail .toolReturnedError) .failed = true := by native_decide

theorem cancelled_delivery_preserves_cancelled_lifecycle :
    closeAndPublishState (.cancelDuringRun .interrupted) .cancelled = true := by native_decide

theorem timed_out_delivery_preserves_timed_out_lifecycle :
    closeAndPublishState .timeout .timedOut 21 = true := by native_decide

def wrongProviderResultRejected : Bool :=
  match acceptAndPublish (world 5) 7 providerTurn providerMessage [] [foregroundAdmission] with
  | .error _ => false
  | .ok accepted => match dispatch accepted 7 permit with
    | .error _ => false
    | .ok dispatched => match ToolDelivery.closeToolOutput dispatched 600
        (.native .complete) toolOutputClose with
      | .error _ => false
      | .ok closed =>
        let wrong := { foregroundResultMessage 1 with
          blocks := [.toolResult 600 "different-provider-id" none
            [.text ⟨⟨700, 0⟩, .composed [.literal [91], .range 0 1, .literal [93]]⟩]] }
        match ToolDelivery.publishToolDelivery closed 600 wrong with
        | .error .publication => true
        | _ => false

theorem result_provider_id_is_bound_to_accepted_intent :
    wrongProviderResultRejected = true := by native_decide

def secondTerminalNotificationRejected : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage []
    [bridgeAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let receipted ← (ToolDelivery.publishBackgroundReceipt backgrounded 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  let terminal ← (terminalize receipted 7 .completed (.message 501)).toOption
  let running ← ownedToolByDocument? terminal 600
  let closed ← (ToolDelivery.closeToolOutput terminal 600
    (.bridge (completedBridge running.context) .bridge_complete) toolOutputClose).toOption
  let delivered ← (ToolDelivery.publishWakeNotification closed 600
    (wakeBinding (wakeNotificationMessage 2)) (wakeNotificationMessage 2)).toOption
  let second := { wakeNotificationMessage 3 with
    header := { (wakeNotificationMessage 3).header with id := 712 }
    key := "second-terminal-notification-600" }
  let secondBinding := { wakeBinding second with
    notificationMessageId := 712
    notificationSequence := 3 }
  pure (match ToolDelivery.publishWakeNotification delivered 600 secondBinding second with
    | .error .publication => true
    | _ => false)

theorem a_physical_tool_has_at_most_one_terminal_notification :
    secondTerminalNotificationRejected = some true := by native_decide

def authenticatedGoalOwnsParentNotification : Bool :=
  match acceptAndPublish (world 5) 7 providerTurn providerMessage [] [foregroundAdmission] with
  | .error _ => false
  | .ok accepted => match dispatch accepted 7 permit with
    | .error _ => false
    | .ok dispatched => match changeToolControl dispatched 7 600 .background with
      | .error _ => false
      | .ok backgrounded => match ToolDelivery.closeToolOutput backgrounded 600
          (.native .complete) toolOutputClose with
        | .error _ => false
        | .ok closed =>
          let rawRejected := match ToolDelivery.publishToolDelivery closed 600
              (toolDeliveryMessage 1) with
            | .error .publication => true
            | _ => false
          let fakeWake : WakeDocumentBinding :=
            { wakeBinding (toolDeliveryMessage 1) with wakeDocument := 10 }
          let parentAsWakeRejected := match ToolDelivery.publishWakeNotification closed 600
              fakeWake (toolDeliveryMessage 1) with
            | .error .publication => true
            | _ => false
          match ToolDelivery.publishGoalNotification closed 600 goalBinding
              (toolDeliveryMessage 1) with
            | .ok delivered => rawRejected && parentAsWakeRejected &&
                delivered.transcript.nextSeq == 2 &&
                delivered.messages.contains (toolDeliveryMessage 1)
            | .error _ => false

theorem typed_goal_owner_publishes_without_background_wake :
    authenticatedGoalOwnsParentNotification = true := by native_decide

def distinctLogicalAdmission : ToolAdmission :=
  ⟨600, { foregroundToolContext with callId := 999 }⟩

def physicalDocumentDoesNotAliasLogicalCallId : Bool :=
  match acceptAndPublish (world 5) 7 providerTurn providerMessage [] [distinctLogicalAdmission] with
  | .error _ => false
  | .ok accepted => match dispatch accepted 7 permit with
    | .error _ => false
    | .ok dispatched => match ownedToolByDocument? dispatched 600 with
      | some tool => tool.document == 600 && tool.context.callId == 999 &&
          (transcriptToolByDocument? dispatched 600).isSome &&
          (transcriptToolByDocument? dispatched 999).isNone
      | none => false

theorem physical_document_is_authority_not_legacy_logical_id :
    physicalDocumentDoesNotAliasLogicalCallId = true := by native_decide

def emptyCancelledClose : Segment :=
  { id := 710, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
    flush := none, close := some (.closed .partial 0 []), createdAt := 5 }

def emptyCancelledResult : MessageEnvelope :=
  { header :=
      { id := 711, session := 1, request := some 10, origin := none
        refs := [], outcome := .complete, role := .user
        publication := .toolDelivery 600 }
    key := "cancelled-tool-600", sequence := 1, nativeId := none
    blocks := [.toolResult 600 "native-call" none []], createdAt := 5 }

def pendingCancellationCanPublishResult : Bool :=
  match acceptAndPublish (world 5) 7 providerTurn providerMessage [] [foregroundAdmission] with
  | .error _ => false
  | .ok accepted => match terminalize accepted 7 .completed (.message 501) with
    | .error _ => false
    | .ok terminal => match ToolDelivery.closeToolOutput terminal 600 .alreadyTerminal
        emptyCancelledClose with
      | .error _ => false
      | .ok closed => match ToolDelivery.publishToolDelivery closed 600
          emptyCancelledResult with
        | .error _ => false
        | .ok delivered =>
          match transcriptToolByDocument? delivered 600 with
          | some row => row.state == .cancelled && row.resultKey.isSome &&
              delivered.transcript.nextSeq == 2
          | none => false

theorem pending_cancellation_has_empty_closure_and_native_result :
    pendingCancellationCanPublishResult = true := by native_decide

def beyondExtentReplayAndCompactedPublication : Bool :=
  match acceptAndPublish (world 5) 7 providerTurn providerMessage [] [foregroundAdmission] with
  | .error _ => false
  | .ok accepted => match dispatch accepted 7 permit with
    | .error _ => false
    | .ok dispatched => match ToolDelivery.closeToolOutput dispatched 600
        (.native .complete) toolOutputClose with
      | .error _ => false
      | .ok closed =>
        let late : Segment :=
          { id := 799, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
            flush := some ⟨1, [⟨0, 1, none⟩], [88]⟩
            close := none, createdAt := 6 }
        let observed := { closed with
          segments := closed.segments ++ [late]
          lease := { closed.lease with now := 6 } }
        match ToolDelivery.closeToolOutput observed 600 .alreadyTerminal toolOutputClose with
        | .error _ => false
        | .ok replayed => match ToolDelivery.publishToolDelivery replayed 600
            { foregroundResultMessage 1 with createdAt := 6 } with
          | .error _ => false
          | .ok delivered =>
            let compacted := { delivered with
              lease := { delivered.lease with now := 9 }
              compactionCursor := some 1 }
            match ToolDelivery.publishToolDelivery compacted 600
                { foregroundResultMessage 1 with createdAt := 6 } with
            | .ok replay => replay == compacted
            | .error _ => false

theorem later_beyond_extent_and_post_compaction_replays_are_inert :
    beyondExtentReplayAndCompactedPublication = true := by native_decide

end CanonicalOutput.Execution.ToolDelivery.Cases
