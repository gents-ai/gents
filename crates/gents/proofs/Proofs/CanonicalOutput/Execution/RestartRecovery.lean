import Proofs.CanonicalOutput.Execution.BackgroundGate
import Proofs.Recovery.Sweeps.BackgroundRestart

/-!
# Native background restart refinement

This adapter is intentionally restricted to the existing orphan observation:
the registry no longer reports an owned childless background execution. This
does not prove that an OS process stopped; ManagedExec cleanup/stop evidence is
a native recovery refinement. Parent expiry alone cannot construct it.
-/

namespace CanonicalOutput.Execution.RestartRecovery

/-- ACP-authenticated projection of one physical AgentToolCall plus the exact
notification rendered by the existing native restart template owner. No
physical document identity is inferred from the legacy logical context. -/
structure RestartBinding where
  document : DocId
  agent : Nat
  session : SessionId
  observation : Recovery.OrphanedBackgroundToolRow
  renderedReason : String
  notification : MessageEnvelope
  authenticated : Bool
  deriving Repr

def restartBindingValid (before : World) (document : DocId)
    (binding : RestartBinding) : Bool :=
  !binding.observation.executionRegistered && binding.authenticated &&
    binding.document == document && binding.agent == before.principal &&
    binding.session == before.sessionId

def closeAction : Recovery.ToolRecoveryCause → ToolExecution.ToolCallContext.Action
  | .deadlineExceeded => .timeout
  | .parentInterrupted | .terminalizeBackgroundedAsInterrupted =>
      .cancelDuringRun .interrupted
  | .unclaimedCrossPrincipalSpawn => .fail .serviceUnavailable
  | _ => .fail .external

def restartEvidence? (observation : Recovery.OrphanedBackgroundToolRow) :
    Option (Recovery.ToolRecoveryCause × Recovery.RestartNotificationObligation) := do
  if ¬ Recovery.orphanedBackgroundToolStale observation then none
  let cause ← Recovery.orphanedBackgroundToolCause observation
  if Recovery.restartDisposition observation.toRestartRow != .terminalize cause then none
  let obligation ← observation.toRestartRow.notification
  pure (cause, obligation)

/-- An absent executor cannot author another output flush. Restart recovery may
only append a conservative Partial closure over the exact tool extent already
committed at this gate; the shared close owner validates that prefix fully. -/
def restartClosingValid (world : World) (tool : OwnedTool) (closing : Segment) : Bool :=
  closing.flush.isNone && closing.coordinate == ⟨tool.requestDoc, .tool tool.document⟩ &&
    closing.writer == .tool tool.document &&
    match closing.close with
    | some (.closed .partial count _) =>
        count == (sourceData world.segments closing.coordinate).length
    | _ => false

theorem restart_evidence_is_orphan_classifier_terminalization
    (observation : Recovery.OrphanedBackgroundToolRow)
    (cause : Recovery.ToolRecoveryCause)
    (obligation : Recovery.RestartNotificationObligation)
    (h : restartEvidence? observation = some (cause, obligation)) :
    Recovery.orphanedBackgroundToolStale observation ∧
      Recovery.restartDisposition observation.toRestartRow = .terminalize cause ∧
      observation.toRestartRow.notification = some obligation := by
  unfold restartEvidence? at h
  split at h
  · contradiction
  · rename_i hstale
    cases hc : Recovery.orphanedBackgroundToolCause observation with
    | none => simp [hc] at h
    | some actualCause =>
        simp [hc] at h
        rcases h with ⟨hclassified, h⟩
        cases hn : Recovery.RestartRow.notification observation.toRestartRow with
        | none => simp [hn] at h
        | some actualObligation =>
            simp [hn] at h
            rcases h with ⟨rfl, rfl⟩
            exact ⟨Classical.not_not.mp hstale, hclassified, rfl⟩

structure Result where
  before : World
  closed : World
  execution : World
  document : DocId
  restartBinding : RestartBinding
  restartBindingTool : OwnedTool
  observation : Recovery.OrphanedBackgroundToolRow
  cause : Recovery.ToolRecoveryCause
  obligation : Recovery.RestartNotificationObligation
  closing : Segment
  message : MessageEnvelope
  wake : SessionQueue.QueueEntry
  notificationBinding : WakeDocumentBinding
  queue : SessionQueue.SessionQueueState
  continuation : BackgroundContinuation.Result
  bindingValid : restartBindingValid before document restartBinding
  closingValid : restartClosingValid before restartBindingTool closing
  evidence : restartEvidence? observation = some (cause, obligation)
  closedBySharedOwner :
    ToolDelivery.closeToolOutput before document (.native (closeAction cause)) closing = .ok closed
  notifiedBySharedOwner :
    BackgroundContinuation.publishAndEnqueue? closed document message wake notificationBinding queue =
      some continuation
  continuationExecution : continuation.execution = execution

def recoverAndNotify? (before : World) (document : DocId)
    (binding : RestartBinding) (closing : Segment)
    (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState) : Option Result :=
  if hv : restartBindingValid before document binding then
    match he : restartEvidence? binding.observation with
  | none => none
  | some (cause, obligation) =>
      match ownedToolByDocument? before document with
      | none => none
      | some tool => if tool.context != binding.observation.call then none else
        if hvclose : restartClosingValid before tool closing then
        if obligation.notificationReason != binding.renderedReason then none else
        match hc : ToolDelivery.closeToolOutput before document
            (.native (closeAction cause)) closing with
        | .error _ => none
        | .ok closed =>
            match hn : BackgroundContinuation.publishAndEnqueue?
                closed document binding.notification wake notificationBinding queue with
            | none => none
            | some continuation => some
                { before := before, closed := closed
                , execution := continuation.execution, document := document
                , restartBinding := binding
                , restartBindingTool := tool
                , observation := binding.observation, cause := cause, obligation := obligation
                , closing := closing, message := binding.notification, wake := wake
                , notificationBinding := notificationBinding
                , queue := queue, continuation := continuation, bindingValid := hv
                , closingValid := hvclose, evidence := he
                , closedBySharedOwner := hc, notifiedBySharedOwner := hn
                , continuationExecution := rfl }
        else none
  else none

theorem successful_recovery_origin
    (before : World) (document : DocId) (binding : RestartBinding) (closing : Segment)
    (wake : SessionQueue.QueueEntry) (notificationBinding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState) (result : Result)
    (h : recoverAndNotify? before document binding closing wake notificationBinding queue =
      some result) :
    result.before = before ∧ result.document = document ∧ result.restartBinding = binding ∧
      result.closing = closing ∧ result.message = binding.notification ∧ result.wake = wake ∧
      result.notificationBinding = notificationBinding ∧ result.queue = queue := by
  unfold recoverAndNotify? at h
  repeat' split at h <;> try contradiction
  all_goals cases h
  all_goals exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩

theorem successful_enqueue_is_actual_notification
    (before : World) (document : DocId) (message : MessageEnvelope)
    (wake : SessionQueue.QueueEntry) (binding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState) (result : BackgroundContinuation.Result)
    (h : BackgroundContinuation.publishAndEnqueue? before document message wake binding queue =
      some result) :
    ToolDelivery.publishWakeNotification before document binding message = .ok result.execution := by
  unfold BackgroundContinuation.publishAndEnqueue? at h
  split at h
  · contradiction
  next execution hp =>
    cases ht : ownedToolByDocument? execution document with
    | none => simp [ht] at h
    | some tool =>
        simp [ht] at h
        rcases h with ⟨_, _, _, _, h⟩
        cases ho : BackgroundContinuation.observeNotification?
            { toolState := tool.context.state
            , notificationMessageId := message.header.id, wake := wake }
            execution.transcript with
        | none => simp [ho] at h
        | some notified =>
          simp [ho] at h
          repeat' split at h <;> try contradiction
          all_goals rcases h with ⟨_, h⟩
          all_goals try contradiction
          all_goals cases h
          all_goals simpa [BackgroundContinuation.publishNotification] using hp

/-- Restart closure and its ordinary background notification commit under the
same storage holder.  The recovery adapter continues to reuse the close,
notification, and queue owners; this wrapper only supplies their shared atomic
boundary and installs the queue result they computed from the current world. -/
def commit (state : World) (actor : Gate.Actor) (now : Time) (document : DocId)
    (binding : RestartBinding) (closing : Segment)
    (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding) : Option World :=
  if state.purpose != .normal || state.gateOwner != some actor ||
      state.gateSchedule.phase != .storage ||
      !StorageWriteGate.pollable state.gateSchedule || now < state.lease.now then none
  else
    let current := Gate.atTime state now
    match recoverAndNotify? current document binding closing wake notificationBinding
        state.queue with
    | none => none
    | some result => some
        { result.execution with
          gateOwner := state.gateOwner
          gateSchedule := { state.gateSchedule with phase := .releasable }
          queue := BackgroundGate.committedQueue state.queue result.continuation }

theorem successful_commit_effect
    (before after : World) (actor : Gate.Actor) (now : Time) (document : DocId)
    (binding : RestartBinding) (closing : Segment) (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding)
    (h : commit before actor now document binding closing wake notificationBinding = some after) :
    ∃ result,
      recoverAndNotify? (Gate.atTime before now) document binding closing wake
          notificationBinding before.queue = some result ∧
      ToolDelivery.closeToolOutput (Gate.atTime before now) document
          (.native (closeAction result.cause)) closing = .ok result.closed ∧
      BackgroundContinuation.publishAndEnqueue? result.closed document
          binding.notification wake notificationBinding before.queue = some result.continuation ∧
      result.continuation.execution = result.execution ∧
      after = { result.execution with
        gateOwner := before.gateOwner
        gateSchedule := { before.gateSchedule with phase := .releasable }
        queue := BackgroundGate.committedQueue before.queue result.continuation } := by
  unfold commit at h
  split at h
  · contradiction
  · cases hr : recoverAndNotify? (Gate.atTime before now) document binding closing wake
        notificationBinding before.queue with
    | none => simp [hr] at h
    | some result =>
        simp [hr] at h
        cases h
        have origin := successful_recovery_origin _ _ _ _ _ _ _ result hr
        refine ⟨result, rfl, ?_, ?_, result.continuationExecution, rfl⟩
        · simpa [origin.1, origin.2.1, origin.2.2.2.1] using
            result.closedBySharedOwner
        · simpa [origin.2.1, origin.2.2.2.2.1, origin.2.2.2.2.2.1,
            origin.2.2.2.2.2.2.1, origin.2.2.2.2.2.2.2] using
            result.notifiedBySharedOwner

theorem successful_commit_preserves_claim_control
    (before after : World) (actor : Gate.Actor) (now : Time) (document : DocId)
    (binding : RestartBinding) (closing : Segment) (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding)
    (h : commit before actor now document binding closing wake notificationBinding = some after) :
    after.requestId = before.requestId ∧ after.sessionId = before.sessionId ∧
      after.claimed = before.claimed ∧ after.retry = before.retry ∧
      after.queue.active = before.queue.active := by
  obtain ⟨result, _, hc, hn, he, hafter⟩ :=
    successful_commit_effect _ _ _ _ _ _ _ _ _ h
  subst after
  have hpublished := successful_enqueue_is_actual_notification
    result.closed document binding.notification wake notificationBinding before.queue
      result.continuation hn
  rw [he] at hpublished
  have hclose := ToolDelivery.tool_write_preserves_request_identity hc
  have hcloseControl := ToolDelivery.tool_write_preserves_composed_control hc
  have hnotify := ToolDelivery.wake_notification_preserves_composed_control
    result.closed result.execution document notificationBinding binding.notification
      hpublished
  have hactive := BackgroundContinuation.successful_enqueue_preserves_active
    result.closed document binding.notification wake notificationBinding before.queue
      result.continuation hn
  exact ⟨hnotify.1.trans hclose.1, hnotify.2.1.trans hclose.2,
    hnotify.2.2.2.1.trans hcloseControl.2.1,
    hnotify.2.2.2.2.trans hcloseControl.2.2,
    by
      cases hq : result.continuation.queued <;>
        simp [hq, BackgroundGate.committedQueue] at hactive ⊢
      exact hactive⟩

theorem successful_commit_nextSequence_monotone
    (before after : World) (actor : Gate.Actor) (now : Time) (document : DocId)
    (binding : RestartBinding) (closing : Segment) (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding)
    (h : commit before actor now document binding closing wake notificationBinding = some after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  obtain ⟨result, _, hc, hn, he, hafter⟩ :=
    successful_commit_effect _ _ _ _ _ _ _ _ _ h
  subst after
  have hpublished := successful_enqueue_is_actual_notification
    result.closed document binding.notification wake notificationBinding before.queue
      result.continuation hn
  rw [he] at hpublished
  have hclose := ToolDelivery.close_preserves_nextSeq _ _ document
    (.native (closeAction result.cause)) closing hc
  have hnotify := ToolDelivery.wake_notification_nextSeq_monotone
    result.closed result.execution document notificationBinding binding.notification hpublished
  exact Nat.le_of_eq (by simpa [Gate.atTime] using hclose.symm) |>.trans hnotify

theorem live_registered_process_cannot_use_restart_adapter
    (before : World) (document : DocId)
    (binding : RestartBinding) (closing : Segment) (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState)
    (hlive : binding.observation.executionRegistered = true) :
    recoverAndNotify? before document binding closing wake notificationBinding queue = none := by
  have he : restartEvidence? binding.observation = none := by
    simp [restartEvidence?, Recovery.orphanedBackgroundToolStale, hlive]
  simp [recoverAndNotify?, restartBindingValid, hlive]

theorem logical_context_alias_cannot_select_another_physical_document
    (before : World) (document : DocId)
    (binding : RestartBinding) (closing : Segment) (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState)
    (hphysical : binding.document ≠ document) :
    recoverAndNotify? before document binding closing wake notificationBinding queue = none := by
  simp [recoverAndNotify?, restartBindingValid, hphysical]

end CanonicalOutput.Execution.RestartRecovery
