import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.BackgroundContinuation

/-! Atomic local-gate composition for notification publication plus its queue owner. -/
namespace CanonicalOutput.Execution.BackgroundGate

def committedQueue (before : SessionQueue.SessionQueueState)
    (result : BackgroundContinuation.Result) : SessionQueue.SessionQueueState :=
  match result.queued with
  | some queued => queued.queue
  | none => before

def commit (state : World) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope) (wake : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding) : Option World :=
  if state.gateOwner != some actor || state.gateSchedule.phase != .storage ||
      !StorageWriteGate.pollable state.gateSchedule ||
      now < state.lease.now then none
  else
    let current := Gate.atTime state now
    match BackgroundContinuation.publishAndEnqueue?
        current document message wake binding state.queue with
    | none => none
    | some result =>
      if result.before != current || result.document != document ||
          result.message != message || result.binding != binding then none else some
        { result.execution with
          gateOwner := state.gateOwner
          gateSchedule := { state.gateSchedule with phase := .releasable }
          queue := committedQueue state.queue result }

theorem other_actor_cannot_commit (state : World) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope) (wake : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding)
    (h : state.gateOwner ≠ some actor) :
    commit state actor now document message wake binding = none := by
  simp [commit, h]

theorem successful_commit_preserves_allocator_monotonicity
    (before after : World) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope) (wake : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding)
    (h : commit before actor now document message wake binding = some after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  unfold commit at h
  split at h
  · contradiction
  · cases hp : BackgroundContinuation.publishAndEnqueue?
        (Gate.atTime before now) document message wake binding before.queue with
    | none => simp [hp] at h
    | some result =>
        simp [hp] at h
        rcases h with ⟨⟨⟨⟨hbefore, hdocument⟩, hmessage⟩, hbinding⟩, rfl⟩
        have htime : before.transcript.nextSeq =
            (Gate.atTime before now).transcript.nextSeq := rfl
        rw [htime]
        apply ToolDelivery.wake_notification_nextSeq_monotone
          (Gate.atTime before now) result.execution document binding message
        simpa [BackgroundContinuation.publishNotification, hbefore, hdocument,
          hmessage, hbinding] using result.published

end CanonicalOutput.Execution.BackgroundGate
