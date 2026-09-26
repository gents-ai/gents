import Proofs.CanonicalOutput.Execution.Handover
import Proofs.CanonicalOutput.Execution.ToolDeliveryCases
import Proofs.CanonicalOutput.Execution.ContinuationCases

namespace CanonicalOutput.Execution.Handover.Cases

open ToolDelivery.Cases
open CanonicalOutput.Execution.Examples

def nextEntry : SessionQueue.QueueEntry :=
  { requestId := 902, createdAt := 6, source := .user, policy := .append
  , queueKey := none, queuedAfter := some 10 }

def nextAdmission (requester : Option Nat := none) : PhysicalRequestAdmission :=
  { document := 801, entry := nextEntry, agent := 1, session := 1
  , requester := requester, authenticated := true }

def nextActivation (requester : Option Nat := none) : Activation :=
  { request := nextAdmission requester, evidence := .ordinary
  , generation := 8, duration := 5, deadline := 11 }

def reacquire (state : World) : Option World := do
  let released ← Gate.scheduling state 1 .release
  let held ← Gate.acquire released 1 true
  pure held

def terminalRunningParent : Option World := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage
    [sessionMessageAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let backgrounded ← (changeToolControl dispatched 7 600 .background).toOption
  let receipted ← (ToolDelivery.publishBackgroundReceipt backgrounded 600
    backgroundReceiptClose backgroundReceiptMessage).toOption
  (terminalize receipted 7 .completed (.message 501)).toOption

def claimedNext : Option World := do
  let parent ← terminalRunningParent
  let gate ← Gate.acquire (Gate.initial parent) 1 true
  let queue : SessionQueue.SessionQueueState :=
    { scope := ⟨1, 1, none⟩, active := none, pending := [nextEntry], terminal := ∅ }
  claimAndActivate { gate with queue := queue, claimed := none } 1 6 nextActivation

def actualHandoverLateToolAndFinish : Option Bool := do
  let claimed ← claimedNext
  let held ← reacquire claimed
  let begun ← beginProcessing held 1 6 8
  let toolHeld ← reacquire begun
  let oldTool ← ownedToolByDocument? toolHeld 600
  let lateClose := { toolOutputClose with createdAt := 7 }
  let closeState ← Gate.commit toolHeld 1 7
    (.toolClose 600 (.native .complete) lateClose)
  let terminalHeld ← reacquire closeState
  let terminal ← Gate.commit terminalHeld 1 7
    (.terminalize 8 .failed .noMessage)
  let finishHeld ← reacquire terminal
  let finished ← finishAndAcknowledge finishHeld 1
  let oldAfter ← ownedToolByDocument? finished.state 600
  pure (finished.state.claimed.isNone && finished.state.queue.active.isNone &&
    oldAfter.context.state == .completed &&
    finished.state.requestId == 801)

theorem old_background_tool_survives_claim_and_closes_before_exact_finish :
    actualHandoverLateToolAndFinish = some true := by native_decide

def requesterMismatchRejected : Bool :=
  match terminalRunningParent with
  | none => false
  | some parent =>
      match Gate.acquire (Gate.initial parent) 1 true with
      | none => false
      | some gate =>
          let queue : SessionQueue.SessionQueueState :=
            { scope := ⟨1, 1, some 33⟩, active := none, pending := [nextEntry], terminal := ∅ }
          (claimAndActivate { gate with queue := queue, claimed := none }
            1 6 nextActivation).isNone

theorem foreign_requester_scope_cannot_activate : requesterMismatchRejected = true := by
  native_decide

def expiredBeginRejected : Option Bool := do
  let claimed ← claimedNext
  let held ← reacquire claimed
  pure ((beginProcessing held 1 12 8).isNone)

theorem begin_uses_current_time_and_rejects_expired_lease :
    expiredBeginRejected = some true := by native_decide

def wakeClaimed : Option World := do
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
    closed 600 message wakeEntry binding ContinuationCases.wakeQueue
  let queued ← composed.queued
  let gate ← Gate.acquire (Gate.initial composed.execution) 1 true
  let request : PhysicalRequestAdmission :=
    { document := binding.wakeDocument, entry := wakeEntry, agent := 1, session := 1
    , requester := none, authenticated := true }
  let snapshot : BackgroundCompletion.WakeAttemptSnapshot :=
    { wakeRequestId := wakeEntry.requestId, throughSequence := 2
    , bindings := [
        { messageId := message.header.id, sequence := message.sequence
        , wakeRequestId := wakeEntry.requestId }] }
  let activation : Activation :=
    { request := request
    , evidence := .backgroundWake snapshot
    , generation := 8
    , duration := 5
    , deadline := 11 }
  claimAndActivate { gate with queue := queued.queue, claimed := none }
    1 6 activation

def finishWake (outcome : RequestExecutionLease.Outcome) : Option FinishResult := do
  let claimed ← wakeClaimed
  let held ← reacquire claimed
  let begun ← beginProcessing held 1 6 8
  let terminalHeld ← reacquire begun
  let terminal ← Gate.commit terminalHeld 1 7
    (.terminalize 8 outcome .noMessage)
  let finishHeld ← reacquire terminal
  finishAndAcknowledge finishHeld 1

def failedWakeReleasesWithoutAcknowledgement : Option Bool := do
  let result ← finishWake .failed
  pure (result.acknowledged.isEmpty && result.state.claimed.isNone &&
    result.state.queue.active.isNone && wakeEntry.requestId ∈ result.state.queue.terminal)

theorem failed_background_wake_releases_claim_without_acknowledging_delivery :
    failedWakeReleasesWithoutAcknowledgement = some true := by native_decide

def completedWakeAcknowledgesExactSnapshot : Option Bool := do
  let result ← finishWake .completed
  pure (result.acknowledged ==
    [{ messageId := (wakeNotificationMessage 2).header.id, sequence := 2
     , wakeRequestId := wakeEntry.requestId }] &&
    result.state.queue.active.isNone)

theorem completed_background_wake_acknowledges_exact_claim_snapshot :
    completedWakeAcknowledgesExactSnapshot = some true := by native_decide

end CanonicalOutput.Execution.Handover.Cases
