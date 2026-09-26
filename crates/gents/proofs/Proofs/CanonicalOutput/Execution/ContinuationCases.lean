import Proofs.CanonicalOutput.Execution.BackgroundGate
import Proofs.CanonicalOutput.Execution.RestartRecovery
import Proofs.CanonicalOutput.Execution.ToolDeliveryCases

/-!
# Canonical background continuation integration cases

These cases exercise the composed owners rather than manufacturing a durable
notification or queue state as evidence of success.  Physical AgentRequest
documents remain distinct from logical queue request ids throughout.
-/

namespace CanonicalOutput.Execution.ContinuationCases

open CanonicalOutput.Execution.Examples
open CanonicalOutput.Execution.ToolDelivery.Cases

def wakeQueue : SessionQueue.SessionQueueState :=
  { scope := ⟨1, 1, none⟩, active := none, pending := [], terminal := ∅ }

def activeWakeQueue : SessionQueue.SessionQueueState :=
  { wakeQueue with active := some wakeEntry.requestId }

def terminalWakeQueue : SessionQueue.SessionQueueState :=
  { wakeQueue with terminal := {wakeEntry.requestId} }

/-- One complete shared-owner trace: accepted provider intent, dispatch,
immediate background receipt, parent completion, caused-request terminal close,
atomic notification+wake publication, claim, and exact replay after both
claim and finish. -/
def wakeContinuationTrace : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage
    [sessionMessageAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let receipted ← (ToolDelivery.publishBackgroundReceipt backgrounded 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  let parentTerminal ← (terminalize receipted 7 .completed (.message 501)).toOption
  let closed ← (ToolDelivery.closeToolOutput parentTerminal 600
    (.native .complete) toolOutputClose).toOption
  let message := wakeNotificationMessage 2
  let binding := wakeBinding message
  let composed ← BackgroundContinuation.publishAndEnqueue?
    closed 600 message wakeEntry binding wakeQueue
  let queued ← composed.queued
  let continuation ← BackgroundContinuation.claimContinuation? composed
  let activeReplay ← BackgroundContinuation.publishAndEnqueue?
    composed.execution 600 message wakeEntry binding continuation.queue
  let finished ← SessionQueue.step? continuation.queue .finishActive
  let terminalReplay ← BackgroundContinuation.publishAndEnqueue?
    composed.execution 600 message wakeEntry binding finished
  let held ← Gate.acquire (Gate.initial closed) 1 true
  let gateCommitted ← BackgroundGate.commit
    { held with queue := wakeQueue } 1 5 600 message wakeEntry binding
  pure (gateCommitted == { composed.execution with
      gateOwner := some 1, gateSchedule := { held.gateSchedule with phase := .releasable },
      queue := queued.queue } &&
    gateCommitted.queue.scope.agent == queued.queue.scope.agent &&
    gateCommitted.queue.sessionId == queued.queue.sessionId &&
    gateCommitted.queue.scope.requester == queued.queue.scope.requester &&
    gateCommitted.queue.active == queued.queue.active &&
    gateCommitted.queue.pending == queued.queue.pending &&
    gateCommitted.queue.terminal.card == queued.queue.terminal.card &&
    continuation.queue.active == some wakeEntry.requestId &&
    activeReplay.execution == composed.execution && activeReplay.queued.isNone &&
    finished.active.isNone && wakeEntry.requestId ∈ finished.terminal &&
    terminalReplay.execution == composed.execution && terminalReplay.queued.isNone &&
    composed.execution.transcript.nextSeq == 3)

theorem accepted_background_completion_claims_and_replays_after_finish :
    wakeContinuationTrace = some true := by native_decide

/-- An authenticated row identity cannot turn a fresh publication into replay
after its logical wake has already become active or terminal. -/
def freshNotificationCannotAttachToConsumedWake : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage
    [sessionMessageAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let receipted ← (ToolDelivery.publishBackgroundReceipt backgrounded 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  let parentTerminal ← (terminalize receipted 7 .completed (.message 501)).toOption
  let closed ← (ToolDelivery.closeToolOutput parentTerminal 600
    (.native .complete) toolOutputClose).toOption
  let message := wakeNotificationMessage 2
  let binding := wakeBinding message
  pure ((BackgroundContinuation.publishAndEnqueue?
      closed 600 message wakeEntry binding activeWakeQueue).isNone &&
    (BackgroundContinuation.publishAndEnqueue?
      closed 600 message wakeEntry binding terminalWakeQueue).isNone)

theorem fresh_notification_is_rejected_for_active_and_terminal_wake :
    freshNotificationCannotAttachToConsumedWake = some true := by native_decide

def wrongWakeBindingsRejected : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage
    [sessionMessageAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let receipted ← (ToolDelivery.publishBackgroundReceipt backgrounded 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  let parentTerminal ← (terminalize receipted 7 .completed (.message 501)).toOption
  let closed ← (ToolDelivery.closeToolOutput parentTerminal 600
    (.native .complete) toolOutputClose).toOption
  let message := wakeNotificationMessage 2
  let physicalAlias := { wakeBinding message with wakeDocument := 801 }
  let logicalAliasEntry := { wakeEntry with requestId := 902 }
  let logicalAlias := { wakeBinding message with entry := logicalAliasEntry }
  pure ((BackgroundContinuation.publishAndEnqueue?
      closed 600 message wakeEntry physicalAlias wakeQueue).isNone &&
    (BackgroundContinuation.publishAndEnqueue?
      closed 600 message wakeEntry logicalAlias wakeQueue).isNone)

theorem physical_wake_document_and_logical_queue_id_are_both_exact :
    wrongWakeBindingsRejected = some true := by native_decide

/-- Goal-owned notification uses the ordinary local gate and never constructs
or mutates a background-completion queue. -/
def goalGateDoesNotCreateWake : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage
    [foregroundAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let closed ← (ToolDelivery.closeToolOutput backgrounded 600
    (.native .complete) toolOutputClose).toOption
  let held ← Gate.acquire (Gate.initial closed) 1 true
  let before := { held with queue := wakeQueue }
  let committed ← Gate.commit before 1 5
    (.toolGoalDeliver 600 goalBinding (toolDeliveryMessage 1))
  pure (committed.messages.contains (toolDeliveryMessage 1) &&
    committed.transcript.nextSeq == 2 &&
    committed.queue.scope.agent == before.queue.scope.agent &&
    committed.queue.sessionId == before.queue.sessionId &&
    committed.queue.scope.requester == before.queue.scope.requester &&
    committed.queue.active == before.queue.active &&
    committed.queue.pending == before.queue.pending &&
    committed.queue.terminal.card == before.queue.terminal.card)

theorem typed_goal_gate_publication_has_no_background_queue_effect :
    goalGateDoesNotCreateWake = some true := by native_decide

def restartObservation (context : ToolExecution.ToolCallContext)
    (registered : Bool := false) : Recovery.OrphanedBackgroundToolRow :=
  { call := context, deadlineExpired := false, unclaimedExpired := false
    parentLive := true, parentInterrupted := false, parentTerminal := false
    executionRegistered := registered, process := .stopped
    ownerTaskDeleted := false }

def restartRawOutput : Segment := { toolOutputClose with close := none }

def restartTerminalClose : Segment :=
  { toolOutputClose with id := 704, flush := none, close := some (.closed .«partial» 1 [1]) }

def restartNotification : MessageEnvelope :=
  { wakeNotificationMessage 1 with
    header := { (wakeNotificationMessage 1).header with refs := [⟨704, 0⟩] },
    blocks := [.text ⟨⟨704, 0⟩, .composed [.literal [91], .range 0 1, .literal [93]]⟩] }

def restartBinding (context : ToolExecution.ToolCallContext)
    (registered : Bool := false) : RestartRecovery.RestartBinding :=
  { document := 600, agent := 1, session := 1
    observation := restartObservation context registered
    renderedReason := "interrupted_on_restart"
    notification := restartNotification
    authenticated := true }

def restartReadyWorld : Option World := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage
    [foregroundAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  (ToolDelivery.appendToolOutput backgrounded 600 restartRawOutput).toOption

/-- The restart adapter consumes an exact physical binding.  A live process
cannot be recovered, and the same legacy logical context cannot select another
physical document.  The valid orphan path closes and then publishes/enqueues
through the same shared owners as a normal late completion. -/
def restartRecoveryCases : Option Bool := do
  let withOutput ← restartReadyWorld
  let tool ← ownedToolByDocument? withOutput 600
  let binding := restartBinding tool.context
  let notificationBinding := wakeBinding restartNotification
  let recovered ← RestartRecovery.recoverAndNotify? withOutput 600 binding
    restartTerminalClose wakeEntry notificationBinding wakeQueue
  let finalTool ← ownedToolByDocument? recovered.execution 600
  let liveBinding := restartBinding tool.context true
  pure (finalTool.context.state == .cancelled &&
    recovered.execution.messages.contains restartNotification &&
    recovered.continuation.queued.isSome &&
    (RestartRecovery.recoverAndNotify? withOutput 600 liveBinding
      restartTerminalClose wakeEntry notificationBinding wakeQueue).isNone &&
    (RestartRecovery.recoverAndNotify? withOutput 601 binding
      restartTerminalClose wakeEntry notificationBinding wakeQueue).isNone &&
    (RestartRecovery.recoverAndNotify? withOutput 600 binding
      toolOutputClose wakeEntry notificationBinding wakeQueue).isNone)

theorem restart_requires_orphaned_process_and_exact_physical_document :
    restartRecoveryCases = some true := by native_decide

def restartGateInput : Option (World × RestartRecovery.RestartBinding) := do
  let ready ← restartReadyWorld
  let tool ← ownedToolByDocument? ready 600
  let held ← Gate.acquire (Gate.initial ready) 1 true
  pure ({ held with queue := wakeQueue }, restartBinding tool.context)

/-- The application boundary commits the wake queue as well as output, admits
only the current holder, and does not turn parent expiry into orphan evidence. -/
def restartGateCase : Option Bool := do
  let (before, binding) ← restartGateInput
  let notificationBinding := wakeBinding restartNotification
  let after ← RestartRecovery.commit before 1 5 600 binding
    restartTerminalClose wakeEntry notificationBinding
  let later ← RestartRecovery.commit before 1 6 600
    { binding with notification := { binding.notification with createdAt := 6 } }
    { restartTerminalClose with createdAt := 6 } wakeEntry notificationBinding
  let tool ← ownedToolByDocument? after 600
  pure (tool.context.state == .cancelled &&
    after.messages.contains restartNotification &&
    after.queue.pending.contains wakeEntry &&
    after.queue.active == before.queue.active &&
    after.gateOwner == some 1 && after.gateSchedule.phase == .releasable &&
    after.lease == before.lease &&
    later.lease == { before.lease with now := 6 } &&
    (RestartRecovery.commit before 2 5 600 binding
      restartTerminalClose wakeEntry notificationBinding).isNone &&
    (RestartRecovery.commit before 1 4 600 binding
      restartTerminalClose wakeEntry notificationBinding).isNone &&
    (RestartRecovery.commit
      { before with gateSchedule := { before.gateSchedule with phase := .releasable } }
      1 5 600 binding restartTerminalClose wakeEntry notificationBinding).isNone &&
    (RestartRecovery.commit
      { before with gateSchedule :=
        { before.gateSchedule with independent := false, siblingWaiting := true } }
      1 5 600 binding restartTerminalClose wakeEntry notificationBinding).isNone &&
    (RestartRecovery.commit before 1 5 600
      { binding with observation := { binding.observation with executionRegistered := true } }
      restartTerminalClose wakeEntry notificationBinding).isNone)

theorem restart_gate_commits_queue_with_clock_only_progress_and_rejects_invalid_admission :
    restartGateCase = some true := by native_decide

end CanonicalOutput.Execution.ContinuationCases
