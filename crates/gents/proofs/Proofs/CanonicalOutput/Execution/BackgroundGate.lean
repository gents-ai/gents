import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.BackgroundContinuation

/-! Atomic local-gate composition for notification publication plus its queue owner. -/
namespace CanonicalOutput.Execution.BackgroundGate

structure State where
  gate : Gate.State
  queue : SessionQueue.SessionQueueState

/-- Lift an ordinary authoritative gate commit into the paired execution/queue
state. The existing gate remains the sole operation owner; this adapter only
records that an unrelated queue is unchanged. -/
def commitExecution (state : State) (actor : Gate.Actor) (now : Time)
    (operation : Gate.Operation) : Option State := do
  let gate ← Gate.commit state.gate actor now operation
  pure { gate := gate, queue := state.queue }

theorem successful_execution_commit_preserves_queue
    (before after : State) (actor : Gate.Actor) (now : Time)
    (operation : Gate.Operation)
    (h : commitExecution before actor now operation = some after) :
    after.queue = before.queue := by
  unfold commitExecution at h
  cases hc : Gate.commit before.gate actor now operation with
  | none => simp [hc] at h
  | some gate => simp [hc] at h; cases h; rfl

def committedQueue (before : SessionQueue.SessionQueueState)
    (result : BackgroundContinuation.Result) : SessionQueue.SessionQueueState :=
  match result.queued with
  | some queued => queued.queue
  | none => before

def commit (state : State) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope) (wake : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding) : Option State :=
  if state.gate.owner != some actor || state.gate.schedule.phase != .storage ||
      !StorageWriteGate.pollable state.gate.schedule ||
      now < state.gate.execution.lease.now then none
  else
    let current := Gate.atTime state.gate.execution now
    match BackgroundContinuation.publishAndEnqueue?
        current document message wake binding state.queue with
    | none => none
    | some result =>
      if result.before != current || result.document != document ||
          result.message != message || result.binding != binding then none else some
        { gate :=
            { execution := result.execution
            , owner := state.gate.owner
            , schedule := { state.gate.schedule with phase := .releasable } }
        , queue := committedQueue state.queue result }

theorem other_actor_cannot_commit (state : State) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope) (wake : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding)
    (h : state.gate.owner ≠ some actor) :
    commit state actor now document message wake binding = none := by
  simp [commit, h]

theorem successful_commit_preserves_allocator_monotonicity
    (before after : State) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope) (wake : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding)
    (h : commit before actor now document message wake binding = some after) :
    before.gate.execution.transcript.nextSeq ≤ after.gate.execution.transcript.nextSeq := by
  unfold commit at h
  split at h
  · contradiction
  · cases hp : BackgroundContinuation.publishAndEnqueue?
        (Gate.atTime before.gate.execution now) document message wake binding before.queue with
    | none => simp [hp] at h
    | some result =>
        simp [hp] at h
        rcases h with ⟨⟨⟨⟨hbefore, hdocument⟩, hmessage⟩, hbinding⟩, rfl⟩
        have htime : before.gate.execution.transcript.nextSeq =
            (Gate.atTime before.gate.execution now).transcript.nextSeq := rfl
        rw [htime]
        apply ToolDelivery.wake_notification_nextSeq_monotone
          (Gate.atTime before.gate.execution now) result.execution document binding message
        simpa [BackgroundContinuation.publishNotification, hbefore, hdocument,
          hmessage, hbinding] using result.published

/-- Composed traces interleave the existing request/tool gate with the atomic
wake-publication operation. Lifting a normal gate trace cannot mutate the queue;
the wake constructor is the only transition here that changes both owners. -/
inductive Trace : State → State → Prop where
  | gate {before after : State}
      (hgate : Gate.Trace before.gate after.gate)
      (hqueue : after.queue = before.queue) : Trace before after
  | wake {before after : State} (actor : Gate.Actor) (now : Time)
      (document : DocId) (message : MessageEnvelope) (entry : SessionQueue.QueueEntry)
      (binding : WakeDocumentBinding)
      (h : commit before actor now document message entry binding = some after) :
      Trace before after
  | trans {first second third : State} : Trace first second → Trace second third →
      Trace first third

theorem Trace.nextSequence_monotone {before after : State} (trace : Trace before after) :
    before.gate.execution.transcript.nextSeq ≤ after.gate.execution.transcript.nextSeq := by
  induction trace with
  | gate hgate _ => exact hgate.nextSequence_monotone
  | wake actor now document message entry binding h =>
      exact successful_commit_preserves_allocator_monotonicity
        _ _ actor now document message entry binding h
  | trans _ _ ihFirst ihSecond => exact Nat.le_trans ihFirst ihSecond

end CanonicalOutput.Execution.BackgroundGate
