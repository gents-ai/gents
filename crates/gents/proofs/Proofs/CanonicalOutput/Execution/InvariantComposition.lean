import Proofs.CanonicalOutput.Execution.SequenceInvariant
import Proofs.CanonicalOutput.Execution.SessionComposition

/-! Preservation over the existing application trace, not a second transition
system or a trace whose constructors assume their own postconditions. -/
namespace CanonicalOutput.Execution.SessionComposition

private theorem claim_sequenceBound (before after : World) (actor : Gate.Actor)
    (now : Time) (activation : Handover.Activation) (bound : SequenceBound before)
    (h : Handover.claimAndActivate before actor now activation = some after) :
    SequenceBound after := by
  rw [Handover.successful_claim_frame before after actor now activation h]
  exact bound

private theorem begin_sequenceBound (before after : World) (actor : Gate.Actor)
    (now : Time) (generation : Generation) (bound : SequenceBound before)
    (h : Handover.beginProcessing before actor now generation = some after) :
    SequenceBound after := by
  rw [Handover.successful_begin_frame before after actor now generation h]
  exact bound

private theorem finish_sequenceBound (before : World) (after : Handover.FinishResult)
    (actor : Gate.Actor) (bound : SequenceBound before)
    (h : Handover.finishAndAcknowledge before actor = some after) :
    SequenceBound after.state := by
  rw [Handover.successful_finish_frame before after actor h]
  exact bound

private theorem goal_sequenceBound (goal : GoalAutomation.OperatorResume.Snapshot)
    (before : World) (actor : Gate.Actor) (now : Time)
    (request : GoalAutomation.OperatorResume.ClaimedRequest)
    (binding : GoalContinuation.Binding) (entry : SessionQueue.QueueEntry)
    (result : GoalContinuation.Result) (bound : SequenceBound before)
    (h : GoalContinuation.publishGoalChild? goal before actor now request binding entry = some result) :
    SequenceBound result.after := by
  unfold GoalContinuation.publishGoalChild? at h
  dsimp only at h
  repeat' split at h <;> try contradiction
  all_goals rcases h with ⟨_, ⟨_, rfl⟩⟩; exact bound

private theorem wake_sequenceBound (before after : World) (actor : Gate.Actor)
    (now : Time) (document : DocId) (message : MessageEnvelope)
    (entry : SessionQueue.QueueEntry) (binding : WakeDocumentBinding)
    (bound : SequenceBound before)
    (h : BackgroundGate.commit before actor now document message entry binding = some after) :
    SequenceBound after := by
  unfold BackgroundGate.commit at h
  split at h
  · contradiction
  · cases hp : BackgroundContinuation.publishAndEnqueue?
        (Gate.atTime before now) document message entry binding before.queue with
    | none => simp [hp] at h
    | some result =>
      simp [hp] at h
      rcases h with ⟨⟨⟨⟨hbefore, hdocument⟩, hmessage⟩, hbinding⟩, rfl⟩
      apply ToolDelivery.publishWakeNotification_preserves_sequenceBound
        (Gate.atTime before now) result.execution document binding message bound
      simpa [BackgroundContinuation.publishNotification, hbefore, hdocument,
        hmessage, hbinding] using result.published

/-- A bound on the initial canonical headers remains true after every admitted
application transition, including physical handover and late tool publication.
The trace contains no invariant premises: preservation is proved here. -/
theorem Trace.sequenceBound {before after : World} (trace : Trace before after)
    (bound : SequenceBound before) : SequenceBound after := by
  induction trace with
  | refl => exact bound
  | provider actor now operation h =>
      have hc := provider_commit_is_actual_gate_commit _ _ actor now operation h
      have hb := Gate.successful_commit_preserves_sequenceBound _ _ actor now _ bound hc
      simpa [SequenceBound] using hb
  | policy before after now operation _ h =>
      rw [CompletionRetry.CanonicalGate.stepPolicy_preserves_gate now operation h]
      exact bound
  | gate before after actor now operation h =>
      unfold CompletionRetry.CanonicalGate.commitGate at h
      split at h <;> try contradiction
      exact Gate.successful_commit_preserves_sequenceBound _ _ actor now _ bound h
  | acquire before after actor independent h =>
      rw [Gate.acquire_preserves_durable_world before after actor independent h]
      exact bound
  | scheduling before after actor event h =>
      rw [Gate.scheduling_preserves_durable_world before after actor event h]
      exact bound
  | activate actor now activation scope budget deadline h =>
      rename_i before after
      unfold SessionComposition.activate at h
      split at h <;> try contradiction
      cases hc : Handover.claimAndActivate before actor now activation with
      | none => simp [hc] at h
      | some world =>
        simp [hc] at h
        cases h
        have hb := claim_sequenceBound before world actor now activation bound hc
        simpa [SequenceBound] using hb
  | activateTitle actor now activation scope budget deadline h =>
      rename_i before after
      unfold SessionComposition.activateTitle at h
      cases hc : Handover.claimTitle before actor now activation with
      | none => simp [hc] at h
      | some world =>
        simp [hc] at h
        cases h
        rw [Handover.successful_title_claim_frame before world actor now activation hc]
        exact bound
  | finish actor acknowledged h =>
      rename_i before after
      unfold SessionComposition.finish at h
      cases hc : Handover.finishAndAcknowledge before actor with
      | none => simp [hc] at h
      | some result =>
        simp [hc] at h
        rcases h with ⟨rfl, rfl⟩
        exact finish_sequenceBound _ _ actor bound hc
  | activateGoal actor now result published generation duration leaseDeadline scope budget deadline h =>
      rename_i before after
      unfold SessionComposition.activateGoal at h
      cases hc : Handover.claimAndActivate before actor now
          (GoalContinuation.childActivation result generation duration leaseDeadline) with
      | none => simp [hc] at h
      | some world =>
        simp [hc] at h
        cases h
        have hb := claim_sequenceBound before world actor now
          (GoalContinuation.childActivation result generation duration
            leaseDeadline) bound hc
        simpa [SequenceBound] using hb
  | beginProcessing before after actor now generation h =>
      exact begin_sequenceBound before after actor now generation bound h
  | wake before after actor now document message entry binding h =>
      exact wake_sequenceBound before after actor now document message entry binding bound h
  | goal before goal actor now request binding entry result h =>
      exact goal_sequenceBound goal before actor now request binding entry result bound h
  | restart before after actor now document binding closing wake notificationBinding h =>
      obtain ⟨result, _, hc, hn, he, rfl⟩ := RestartRecovery.successful_commit_effect
        before after actor now document binding closing wake notificationBinding h
      have frame := ToolDelivery.close_preserves_publications _ _ document
        (.native (RestartRecovery.closeAction result.cause)) closing hc
      have closedBound : SequenceBound result.closed :=
        bound.of_messages_eq frame.2.1 frame.1 (Nat.le_of_eq frame.2.2.symm)
      have published := RestartRecovery.successful_enqueue_is_actual_notification
        result.closed document binding.notification wake notificationBinding before.queue
        result.continuation hn
      have finalBound := ToolDelivery.publishWakeNotification_preserves_sequenceBound
        result.closed result.continuation.execution document notificationBinding
        binding.notification closedBound published
      simpa only [he] using finalBound
  | trans _ _ first second => exact second (first bound)

end CanonicalOutput.Execution.SessionComposition
