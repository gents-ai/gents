import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.BackgroundContinuation
import Proofs.CanonicalOutput.Execution.Examples

namespace CanonicalOutput.Execution.Gate.Cases

open CanonicalOutput.Execution.Examples

opaque renewalBeforeRecovery : Option Bool := do
  let held ← acquire (initial (world 5)) 1 true
  let renewed ← commit held 1 8 (.renew 7 10)
  let blocked := (acquire renewed 2 true).isNone
  let released ← scheduling renewed 1 .release
  let recoveryHeld ← acquire released 2 true
  pure (blocked && (commit recoveryHeld 2 10 (.recover 7 8 5 20 [])).isNone)

opaque outputAloneDoesNotRenew : Option Bool := do
  let held ← acquire (initial (world 5)) 1 true
  let appended ← commit held 1 5 (.append 7 (raw 100 0 0 5))
  let released ← scheduling appended 1 .release
  let recoveryHeld ← acquire released 2 true
  let items := [RecoveryItem.mk (partialClose 101 0 1 10)
    (some (recoveryMessage 200 101 0 10))]
  let recovered ← commit recoveryHeld 2 10 (.recover 7 8 5 20 items)
  pure (recovered.currentGeneration? == some 8)

/-- Under the scheduling premise that the owner of a published, dispatched tool
intent reaches its held write gate at the due time, it renews explicitly before
yielding. This composes the transcript intent, not the external tool scheduler,
and uses no tool output as heartbeat evidence. -/
opaque toolWaitExplicitRenewal : Option Bool := do
  let acceptHeld ← acquire (initial (routedWorld 5)) 1 true
  let accepted ← commit acceptHeld 1 5
    (.accept 7 providerTurn providerMessage [remote] [remoteAdmission])
  let acceptReleased ← scheduling accepted 1 .release
  let dispatchHeld ← acquire acceptReleased 1 true
  let dispatched ← commit dispatchHeld 1 5 (.dispatch 7 permit)
  let dispatchReleased ← scheduling dispatched 1 .release
  let renewalHeld ← acquire dispatchReleased 1 false
  let renewed ← commit renewalHeld 1 8 (.renew 7 10)
  let released ← scheduling renewed 1 .release
  pure (renewed.lease.lease == .active 7 5 13 &&
    physicalRunning renewed 600 &&
    !(600 ∈ renewed.transcript.inFlight) && released.gateOwner.isNone)

opaque recoveryBeforeStaleWriter : Option Bool := do
  let held ← acquire (initial (world 5)) 1 true
  let appended ← commit held 1 5 (.append 7 (raw 100 0 0 5))
  let released ← scheduling appended 1 .release
  let recoveryHeld ← acquire released 2 true
  let items := [RecoveryItem.mk (partialClose 101 0 1 10) (some (recoveryMessage 200 101 0 10))]
  let recovered ← commit recoveryHeld 2 10 (.recover 7 8 5 20 items)
  let releasedAgain ← scheduling recovered 2 .release
  let staleHeld ← acquire releasedAgain 1 true
  let late : Segment := { raw 102 0 1 11 with flush := some ⟨1, [⟨0, 1, none⟩], [66]⟩ }
  pure ((commit staleHeld 1 11 (.append 7 late)).isNone &&
    recovered.currentGeneration? == some 8)

opaque sameTaskWaitDoesNotCommit : Option Bool := do
  let held ← acquire (initial (world 5)) 1 false
  let suspended ← scheduling held 1 .siblingWait
  pure ((commit suspended 1 5 (.append 7 (raw 100 0 0 5))).isNone &&
    suspended.segments.isEmpty)

/-- The truncation counterexample is reached through admitted raw appends,
not by assuming an arbitrary malformed initial collection. -/
opaque completeTruncationRejected : Option Bool := do
  let first ← acquire (initial (world 5)) 1 true
  let firstCommitted ← commit first 1 5 (.append 7 providerFirstFlush)
  let firstReleased ← scheduling firstCommitted 1 .release
  let second ← acquire firstReleased 1 true
  let secondCommitted ← commit second 1 5 (.append 7 providerSecondFlush)
  let secondReleased ← scheduling secondCommitted 1 .release
  let publication ← acquire secondReleased 1 true
  pure ((commit publication 1 5
    (.accept 7 shortProviderClose shortProviderMessage [] [remoteAdmission])).isNone)

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
    key := "tool-result-600", sequence := sequence, nativeId := none
    blocks := [.text ⟨⟨700, 0⟩, .composed [.literal [91], .range 0 1, .literal [93]]⟩]
    createdAt := 5 }

def wakeEntry : SessionQueue.QueueEntry :=
  { requestId := 11, createdAt := 5, source := .backgroundCompletion
    policy := .coalesce, queueKey := some 1, queuedAfter := none }

def wakeBinding : WakeDocumentBinding :=
  { entry := wakeEntry, agent := 1, session := 1
    notificationMessageId := 701, notificationSequence := 2
    wakeDocument := 11, authenticated := true }

def wakeQueue : SessionQueue.SessionQueueState :=
  { scope := ⟨1, 1, none⟩, active := none, pending := [], terminal := ∅ }

def wakeNotificationMessage : MessageEnvelope :=
  { toolDeliveryMessage 2 with
    header := { (toolDeliveryMessage 2).header with request := some 11 } }

def foregroundResultMessage (sequence : Nat := 1) : MessageEnvelope :=
  { toolDeliveryMessage sequence with
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

def releaseAndAcquire (state : World) (actor : Actor := 1) : Option World := do
  let released ← scheduling state actor .release
  acquire released actor true

/-- The foreground lifecycle is one chain of real gate commits. Delivery is
not inferred from terminal state: close and publication are separate composed
owner operations, and parent completion occurs only afterward. -/
opaque foregroundEndToEnd : Option Bool := do
  let acceptHeld ← acquire (initial (world 5)) 1 true
  let accepted ← commit acceptHeld 1 5
    (.accept 7 providerTurn providerMessage [] [foregroundAdmission])
  let dispatchHeld ← releaseAndAcquire accepted
  let dispatched ← commit dispatchHeld 1 5 (.dispatch 7 permit)
  let closeHeld ← releaseAndAcquire dispatched
  let closed ← commit closeHeld 1 5
    (.toolClose 600 (.native .complete) toolOutputClose)
  let deliveryHeld ← releaseAndAcquire closed
  let delivered ← commit deliveryHeld 1 5
    (.toolDeliver 600 (foregroundResultMessage 1))
  let replayHeld ← releaseAndAcquire delivered
  let replayed ← commit replayHeld 1 5
    (.toolDeliver 600 (foregroundResultMessage 1))
  let terminalHeld ← releaseAndAcquire replayed
  let terminal ← commit terminalHeld 1 5
    (.terminalize 7 .completed (.message 501))
  pure (physicalRunning dispatched 600 &&
    terminal.lease.request == .completed &&
    terminal.transcript.nextSeq == 2 &&
    terminal.messages.contains (foregroundResultMessage 1) &&
    replayed == delivered)

opaque foregroundCompletionWhileRunningRejected : Option Bool := do
  let acceptHeld ← acquire (initial (world 5)) 1 true
  let accepted ← commit acceptHeld 1 5
    (.accept 7 providerTurn providerMessage [] [foregroundAdmission])
  let dispatchHeld ← releaseAndAcquire accepted
  let dispatched ← commit dispatchHeld 1 5 (.dispatch 7 permit)
  let terminalHeld ← releaseAndAcquire dispatched
  pure ((commit terminalHeld 1 5
    (.terminalize 7 .completed (.message 501))).isNone)

@[noinline] opaque publishWakeSummary (before : World)
    (expectedLease : RequestExecutionLease.World Generation) : Option Bool := do
  let delivered ← BackgroundContinuation.publishAndEnqueue? before 600
    wakeNotificationMessage wakeEntry wakeBinding wakeQueue
  pure (delivered.execution.lease == expectedLease && delivered.queued.isSome &&
    delivered.execution.transcript.nextSeq == 3)

/-- Backgrounding is an explicit fenced transition. The parent then completes,
and the still-running tool closes and publishes afterward without reviving it. -/
opaque backgroundAccepted : Option World := do
  let acceptHeld ← acquire (initial (world 5)) 1 true
  commit acceptHeld 1 5 (.accept 7 providerTurn providerMessage [] [foregroundAdmission])

opaque backgroundDispatched : Option World := do
  let accepted ← backgroundAccepted
  let dispatchHeld ← releaseAndAcquire accepted
  commit dispatchHeld 1 5 (.dispatch 7 permit)

opaque backgroundControlled : Option World := do
  let dispatched ← backgroundDispatched
  let backgroundHeld ← releaseAndAcquire dispatched
  commit backgroundHeld 1 5 (.toolControl 7 600 .background)

opaque backgroundReceipted : Option World := do
  let backgrounded ← backgroundControlled
  let receiptHeld ← releaseAndAcquire backgrounded
  commit receiptHeld 1 5
    (.backgroundReceipt 600 backgroundReceiptClose backgroundReceiptMessage)

opaque backgroundTerminal : Option World := do
  let receipted ← backgroundReceipted
  let terminalHeld ← releaseAndAcquire receipted
  commit terminalHeld 1 5
    (.terminalize 7 .completed (.message 501))

opaque backgroundClosed : Option World := do
  let terminal ← backgroundTerminal
  let closeHeld ← releaseAndAcquire terminal
  commit closeHeld 1 5
    (.toolClose 600 (.native .complete) toolOutputClose)

opaque backgroundLateDelivery : Option Bool := do
  let terminal ← backgroundTerminal
  let closed ← backgroundClosed
  let delivered ← publishWakeSummary closed terminal.lease
  pure (terminal.lease.request == .completed &&
    delivered)

opaque recoveryCancelsPendingAtomically : Option Bool := do
  let acceptHeld ← acquire (initial (routedWorld 5)) 1 true
  let accepted ← commit acceptHeld 1 5
    (.accept 7 providerTurn providerMessage [remote] [remoteAdmission])
  let releasedForRecovery ← scheduling accepted 1 .release
  let recoveryHeld ← acquire releasedForRecovery 2 true
  let recovered ← commit recoveryHeld 2 10 (.recover 7 8 5 20 [])
  let tool ← ownedToolByDocument? recovered 600
  let released ← scheduling recovered 2 .release
  let staleHeld ← acquire released 1 true
  pure (tool.context.state == .cancelled &&
    recovered.currentGeneration? == some 8 &&
    (commit staleHeld 1 10 (.dispatch 7 permit)).isNone &&
    !remoteExecutionAdmitted recovered 600)

opaque recoveryHandsOffRunning : Option Bool := do
  let acceptHeld ← acquire (initial (world 5)) 1 true
  let accepted ← commit acceptHeld 1 5
    (.accept 7 providerTurn providerMessage [] [foregroundAdmission])
  let dispatchHeld ← releaseAndAcquire accepted
  let dispatched ← commit dispatchHeld 1 5 (.dispatch 7 permit)
  let releasedForRecovery ← scheduling dispatched 1 .release
  let recoveryHeld ← acquire releasedForRecovery 2 true
  let recovered ← commit recoveryHeld 2 10 (.recover 7 8 5 20 [])
  let tool ← ownedToolByDocument? recovered 600
  pure (tool.context.state == .running && tool.stuckSince == some 10 &&
    tool.cancelCascadeIntentAt == some 10 &&
    !(600 ∈ recovered.transcript.inFlight) &&
    recovered.currentGeneration? == some 8)

def foreignProviderTurn : Segment :=
  { providerTurn with coordinate := ⟨11, .provider 0 1 0⟩ }

def foreignProviderHeader : Header :=
  { providerMessage.header with request := some 11 }

def foreignProviderMessage : MessageEnvelope :=
  { providerMessage with
    header := foreignProviderHeader
    key := "foreign-provider"
    nativeId := some "foreign-native"
    blocks := [.text ⟨⟨500, 0⟩, .full⟩,
      .toolCall 900 "native-call" none "child" ⟨⟨500, 1⟩, .full⟩ none none] }

def foreignAdmission : ToolAdmission :=
  ⟨900, { foregroundToolContext with callId := 900, requestId := 11 }, none⟩

def foreignAccepted : World :=
  match acceptAndPublish { world 5 with requestId := 11 } 7 foreignProviderTurn
      foreignProviderMessage [] [foreignAdmission] with
  | .ok accepted => accepted
  | .error _ => world 5

def currentWithForeignAccepted : World :=
  { foreignAccepted with requestId := 10, lease := lease 5 }

opaque foreignSameGenerationUnchanged : Bool :=
  match ownedToolByDocument? currentWithForeignAccepted 900 with
  | none => false
  | some before =>
      match terminalize currentWithForeignAccepted 7 .completed .noMessage with
      | .error _ => false
      | .ok terminal =>
          match ownedToolByDocument? terminal 900 with
          | none => false
          | some afterTerminal =>
              let expired := { currentWithForeignAccepted with
                lease := { currentWithForeignAccepted.lease with now := 10 } }
              match recoverExpiredBatch expired 7 8 5 20 [] with
              | .error _ => false
              | .ok recovered =>
                  ownedToolByDocument? recovered 900 == some before &&
                    afterTerminal == before

def spawnProviderTurn : Segment :=
  { providerTurn with flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩,
       ⟨1, 2, some
         { block := 1
           part := 0
           kind := .arguments
           tool := some ⟨"native-call", none, "spawn_process"⟩ }⟩], [65, 123, 125]⟩ }

def spawnProviderMessage : MessageEnvelope :=
  { providerMessage with blocks := [.text ⟨⟨500, 0⟩, .full⟩,
      .toolCall 600 "native-call" none "spawn_process" ⟨⟨500, 1⟩, .full⟩ none none] }

def spawnedContext : ToolExecution.ToolCallContext :=
  { callId := 601, requestId := 10, state := .pending
    operation := .nativeCommand, deadline := 30, currentTime := 5
    persistence := .committed, awaitMode := .background }

def spawnedAdmission : SpawnedToolAdmission := ⟨601, 600, spawnedContext⟩

opaque spawnedRunningCase : Option World := do
  let accepted ← acceptAndPublish (world 5) 7 spawnProviderTurn spawnProviderMessage []
    [foregroundAdmission] |>.toOption
  let parentRunning ← dispatch accepted 7 permit |>.toOption
  let spawned ← admitSpawnedBackground parentRunning 7 spawnedAdmission |>.toOption
  dispatch spawned 7 ⟨601, true, true⟩ |>.toOption

opaque spawnedReplayAfterDispatchCase : Bool :=
  match spawnedRunningCase with
  | none => false
  | some childRunning =>
      match admitSpawnedBackground childRunning 7 spawnedAdmission with
      | .ok replayed => replayed == childRunning
      | .error _ => false

opaque spawnedRecoveredCase : Option World := do
  let childRunning ← spawnedRunningCase
  let expired := { childRunning with lease := { childRunning.lease with now := 10 } }
  recoverExpiredBatch expired 7 8 5 20 [] |>.toOption

opaque spawnedReplayAfterRecoveryCase : Bool :=
  match spawnedRecoveredCase with
  | none => false
  | some recovered =>
      match admitSpawnedBackground recovered 7 spawnedAdmission with
      | .ok replayed => replayed == recovered
      | .error _ => false

opaque spawnedConflictRejectedCase : Bool :=
  match spawnedRunningCase with
  | none => false
  | some childRunning =>
      match admitSpawnedBackground childRunning 7
          { spawnedAdmission with document := 602 } with
      | .error _ => true
      | .ok _ => false

def spawnedAdmissionReplayCases : Bool :=
  spawnedReplayAfterDispatchCase && spawnedReplayAfterRecoveryCase &&
    spawnedConflictRejectedCase

def corruptTwinBase : Segment :=
  { providerTurn with id := 504, close := none }

def corruptTwin : Segment :=
  { corruptTwinBase with flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩,
       ⟨1, 2, some
         { block := 1
           part := 0
           kind := .arguments
           tool := some ⟨"native-call", none, "child"⟩ }⟩], [66, 123, 125]⟩ }

opaque corruptAcceptedCase : Option World :=
  acceptAndPublish (world 5) 7 providerTurn providerMessage [] [foregroundAdmission] |>.toOption

opaque corruptPendingRevocationCase : Option (World × World) := do
  let accepted ← corruptAcceptedCase
  let corrupt := { accepted with segments := accepted.segments ++ [corruptTwin] }
  let revoked ← revokeCorrupt corrupt 7 8 .dead (.message 501) |>.toOption
  pure (corrupt, revoked)

opaque corruptRunningRevocationCase : Option World := do
  let accepted ← corruptAcceptedCase
  let running ← dispatch accepted 7 permit |>.toOption
  let corrupt := { running with segments := running.segments ++ [corruptTwin] }
  revokeCorrupt corrupt 7 8 .dead (.message 501) |>.toOption

opaque corruptLateCloseCase : Option (World × World) := do
  let revoked ← corruptRunningRevocationCase
  let closed ← ToolDelivery.closeToolOutput revoked 600
    (.native .complete) toolOutputClose |>.toOption
  pure (revoked, closed)

opaque corruptPendingChecks : Bool :=
  match corruptPendingRevocationCase with
  | none => false
  | some (corrupt, revoked) =>
      revoked.segments == corrupt.segments && revoked.messages == corrupt.messages &&
        (ownedToolByDocument? revoked 600).any
          (fun tool => tool.context.state == .cancelled)

opaque corruptRunningAndCloseChecks : Bool :=
  match corruptLateCloseCase with
  | none => false
  | some (revoked, closed) =>
      (ownedToolByDocument? revoked 600).any (fun tool =>
        tool.context.state == .running && tool.stuckSince == some 5) &&
      (ownedToolByDocument? closed 600).any
        (fun tool => tool.context.state == .completed) &&
      closed.lease == revoked.lease && !(600 ∈ revoked.transcript.inFlight)

def corruptRevocationCases : Bool :=
  corruptPendingChecks && corruptRunningAndCloseChecks

def allGateCaseChecks : Bool :=
  [ renewalBeforeRecovery == some true
  , outputAloneDoesNotRenew == some true
  , toolWaitExplicitRenewal == some true
  , recoveryBeforeStaleWriter == some true
  , sameTaskWaitDoesNotCommit == some true
  , completeTruncationRejected == some true
  , foregroundEndToEnd == some true
  , foregroundCompletionWhileRunningRejected == some true
  , backgroundLateDelivery == some true
  , recoveryCancelsPendingAtomically == some true
  , recoveryHandsOffRunning == some true
  , foreignSameGenerationUnchanged
  , spawnedAdmissionReplayCases
  , corruptRevocationCases ].all id

/- One native compilation evaluates every executable seam regression. Keeping
the checks in one witness avoids recompiling the broad Gate operation closure
once per theorem while retaining each exact Boolean assertion above. -/
theorem all_gate_case_regressions_hold : allGateCaseChecks = true := by native_decide

end CanonicalOutput.Execution.Gate.Cases
