import Proofs.CanonicalOutput.Execution.BackgroundContinuation
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

theorem successful_restart_recovery_uses_existing_owners
    (before : World) (document : DocId)
    (binding : RestartBinding) (closing : Segment) (wake : SessionQueue.QueueEntry)
    (notificationBinding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState) (result : Result)
    (_h : recoverAndNotify? before document binding closing wake notificationBinding queue = some result) :
    restartBindingValid result.before result.document result.restartBinding ∧
      restartClosingValid result.before result.restartBindingTool result.closing ∧
      restartEvidence? result.observation = some (result.cause, result.obligation) ∧
      ToolDelivery.closeToolOutput result.before result.document
          (.native (closeAction result.cause)) result.closing = .ok result.closed ∧
      BackgroundContinuation.publishAndEnqueue? result.closed result.document
        result.message result.wake result.notificationBinding result.queue = some result.continuation ∧
      result.continuation.execution = result.execution :=
  ⟨result.bindingValid, result.closingValid, result.evidence, result.closedBySharedOwner,
    result.notifiedBySharedOwner,
    result.continuationExecution⟩

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
