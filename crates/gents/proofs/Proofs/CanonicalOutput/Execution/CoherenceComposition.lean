import Proofs.CanonicalOutput.Execution.CoherenceInvariant
import Proofs.CanonicalOutput.Execution.RequestCoherence
import Proofs.CanonicalOutput.Execution.ToolCoherence
import Proofs.CanonicalOutput.Execution.ToolCloseCoherence
import Proofs.CanonicalOutput.Execution.AccountingInvariant
import Proofs.CanonicalOutput.Execution.SessionComposition

namespace CanonicalOutput.Execution

theorem Gate.evaluate_preserves_toolProjectionCoherent (operation : Gate.Operation)
    (before after : World) (coherent : toolProjectionCoherent before = true)
    (h : Gate.evaluate operation before = .ok after) : toolProjectionCoherent after = true := by
  cases operation with
  | renew generation deadline =>
    exact renew_preserves_toolProjectionCoherent before after generation deadline coherent
      (mapError_success Gate.Error.execution _ _ h)
  | append generation record =>
    exact appendRaw_preserves_toolProjectionCoherent before after generation record coherent
      (mapError_success Gate.Error.execution _ _ h)
  | closeAuxiliary generation closing =>
    exact closeAuxiliary_preserves_toolProjectionCoherent before after generation closing coherent
      (mapError_success Gate.Error.execution _ _ h)
  | retract generation record =>
    exact retractBeforeRetry_preserves_toolProjectionCoherent before after generation record coherent
      (mapError_success Gate.Error.execution _ _ h)
  | accept generation closing message targets admissions =>
    exact (accepted_publication_is_composed_atomically before after generation closing
      message targets admissions (mapError_success Gate.Error.execution _ _ h)).2.2.1
  | authored generation closing message =>
    exact publishAuthored_preserves_toolProjectionCoherent before after generation closing
      message coherent (mapError_success Gate.Error.execution _ _ h)
  | headerOnly generation message admissions =>
    exact (header_only_publication_is_atomic before after generation message admissions
      (mapError_success Gate.Error.execution _ _ h)).2.2.2
  | dispatch generation permit =>
    exact (dispatch_requires_committed_intent_and_marks_running before after generation permit
      (mapError_success Gate.Error.execution _ _ h)).2.2
  | admitSpawned generation admission =>
    exact (spawned_background_admission_is_owned_without_fabricated_intent before after generation
      admission (mapError_success Gate.Error.execution _ _ h)).1
  | toolControl generation document action =>
    exact tool_control_success_preserves_projection_coherence before after generation document action
      (mapError_success Gate.Error.execution _ _ h)
  | toolAppend document record =>
    exact ToolDelivery.appendToolOutput_preserves_toolProjectionCoherent before after document record
      coherent (mapError_success Gate.Error.delivery _ _ h)
  | toolClose document authority record =>
    exact ToolDelivery.closeToolOutput_preserves_toolProjectionCoherent before after document authority record
      coherent (mapError_success Gate.Error.delivery _ _ h)
  | toolComplete document authority record message =>
    obtain ⟨closed, hclose, hdeliver⟩ := ToolDelivery.completeAndDeliver_success
      before after document authority record message
      (mapError_success Gate.Error.delivery _ _ h)
    exact ToolDelivery.publishToolDelivery_success_toolProjectionCoherent
      closed after document message hdeliver
  | toolDeliver document message =>
    exact ToolDelivery.publishToolDelivery_success_toolProjectionCoherent before after document message
      (mapError_success Gate.Error.delivery _ _ h)
  | toolGoalDeliver document binding message =>
    exact ToolDelivery.publishGoalNotification_success_toolProjectionCoherent before after document binding message
      (mapError_success Gate.Error.delivery _ _ h)
  | backgroundReceipt document closing message =>
    exact ToolDelivery.publishBackgroundReceipt_success_toolProjectionCoherent before after document closing message
      (mapError_success Gate.Error.delivery _ _ h)
  | compact cursor =>
    unfold Gate.evaluate at h
    cases hc : Compaction.advanceCursor? before cursor with
    | none => simp [hc] at h
    | some post =>
      simp [hc] at h
      subst after
      exact advanceCursor_preserves_toolProjectionCoherent before post cursor coherent hc
  | recover expected fresh duration deadline items =>
    exact (recovery_is_all_sources_single_winner_and_exact before after expected fresh duration deadline
      items (mapError_success Gate.Error.execution _ _ h)).2.1
  | recoverTerminal expected fresh outcome selection items =>
    have hp := checked_success _ _ _ (mapError_success Gate.Error.execution _ _ h)
    simp only [Bool.and_eq_true] at hp
    exact hp.1.1.2
  | closePartial generation item =>
    have hp := checked_success _ _ _ (mapError_success Gate.Error.execution _ _ h)
    simp only [Bool.and_eq_true] at hp
    exact hp.1.2
  | revoke expected fresh outcome selection =>
    exact revokeCorrupt_preserves_toolProjectionCoherent before after expected fresh outcome selection
      coherent (mapError_success Gate.Error.execution _ _ h)
  | terminalize generation outcome selection =>
    exact (terminal_selection_commits_with_lifecycle before after generation outcome selection
      (mapError_success Gate.Error.execution _ _ h)).2.2.2

theorem Gate.successful_commit_preserves_toolProjectionCoherent
    (before after : World) (actor : Gate.Actor) (now : Time) (operation : Gate.Operation)
    (coherent : toolProjectionCoherent before = true)
    (h : Gate.commit before actor now operation = some after) :
    toolProjectionCoherent after = true := by
  obtain ⟨execution, heval, rfl⟩ := Gate.commit_reads_current_world before after actor now operation h
  exact Gate.evaluate_preserves_toolProjectionCoherent operation (Gate.atTime before now)
    execution coherent heval

namespace SessionComposition

private theorem wake_coherent (before after : World) (actor : Gate.Actor)
    (now : Time) (document : DocId) (message : MessageEnvelope)
    (entry : SessionQueue.QueueEntry) (binding : WakeDocumentBinding)
    (h : BackgroundGate.commit before actor now document message entry binding = some after) :
    toolProjectionCoherent after = true := by
  unfold BackgroundGate.commit at h
  split at h <;> try contradiction
  cases hp : BackgroundContinuation.publishAndEnqueue?
      (Gate.atTime before now) document message entry binding before.queue with
  | none => simp [hp] at h
  | some result =>
    simp [hp] at h
    rcases h with ⟨⟨⟨⟨hbefore, hdocument⟩, hmessage⟩, hbinding⟩, rfl⟩
    apply ToolDelivery.publishWakeNotification_success_toolProjectionCoherent
      (Gate.atTime before now) result.execution document binding message
    simpa [BackgroundContinuation.publishNotification, hbefore, hdocument, hmessage,
      hbinding] using result.published

private theorem goal_coherent (goal : GoalAutomation.OperatorResume.Snapshot)
    (before : World) (actor : Gate.Actor) (now : Time)
    (request : GoalAutomation.OperatorResume.ClaimedRequest)
    (binding : GoalContinuation.Binding) (entry : SessionQueue.QueueEntry)
    (result : GoalContinuation.Result) (coherent : toolProjectionCoherent before = true)
    (h : GoalContinuation.publishGoalChild? goal before actor now request binding entry = some result) :
    toolProjectionCoherent result.after = true := by
  unfold GoalContinuation.publishGoalChild? at h
  dsimp only at h
  repeat' split at h <;> try contradiction
  all_goals rcases h with ⟨_, ⟨_, rfl⟩⟩; exact coherent

/-- The full tool projection remains coherent across the one application trace,
including request handover, recovery, background delivery and gated restart.
No constructor assumes the invariant it is meant to preserve. -/
theorem Trace.toolProjectionCoherent {before after : World} (trace : Trace before after)
    (coherent : Execution.toolProjectionCoherent before = true) :
    Execution.toolProjectionCoherent after = true := by
  induction trace with
  | refl => exact coherent
  | provider actor now operation h =>
    have post := Gate.successful_commit_preserves_toolProjectionCoherent _ _ actor now _ coherent
      (provider_commit_is_actual_gate_commit _ _ actor now operation h)
    simpa [toolProjectionCoherent] using post
  | policy before after now operation _ h =>
    rw [CompletionRetry.CanonicalGate.stepPolicy_preserves_gate now operation h]
    exact coherent
  | gate before after actor now operation h =>
    unfold CompletionRetry.CanonicalGate.commitGate at h
    split at h <;> try contradiction
    exact Gate.successful_commit_preserves_toolProjectionCoherent _ _ actor now _ coherent h
  | acquire before after actor independent h =>
    rw [Gate.acquire_preserves_durable_world before after actor independent h]
    exact coherent
  | scheduling before after actor event h =>
    rw [Gate.scheduling_preserves_durable_world before after actor event h]
    exact coherent
  | activate actor now activation scope budget deadline h =>
    rename_i before after
    unfold SessionComposition.activate at h
    split at h <;> try contradiction
    cases hc : Handover.claimAndActivate before actor now activation with
    | none => simp [hc] at h
    | some world =>
      simp [hc] at h
      cases h
      rw [Handover.successful_claim_frame before world actor now activation hc]
      exact coherent
  | finish actor acknowledged h =>
    rename_i before after
    unfold SessionComposition.finish at h
    cases hc : Handover.finishAndAcknowledge before actor with
    | none => simp [hc] at h
    | some result =>
      simp [hc] at h
      rcases h with ⟨rfl, rfl⟩
      rw [Handover.successful_finish_frame before result actor hc]
      exact coherent
  | activateGoal actor now result published routes authenticated generation duration leaseDeadline scope budget deadline h =>
    rename_i before after
    unfold SessionComposition.activateGoal at h
    cases hc : Handover.claimAndActivate before actor now
        (GoalContinuation.childActivation result routes authenticated generation duration leaseDeadline) with
    | none => simp [hc] at h
    | some world =>
      simp [hc] at h
      cases h
      rw [Handover.successful_claim_frame before world actor now _ hc]
      exact coherent
  | beginProcessing before after actor now generation h =>
    rw [Handover.successful_begin_frame before after actor now generation h]
    exact coherent
  | wake before after actor now document message entry binding h =>
    exact wake_coherent before after actor now document message entry binding h
  | goal before goal actor now request binding entry result h =>
    exact goal_coherent goal before actor now request binding entry result coherent h
  | restart before after actor now document binding closing wake notificationBinding h =>
    obtain ⟨result, _, _, hn, he, rfl⟩ := RestartRecovery.successful_commit_effect
      before after actor now document binding closing wake notificationBinding h
    have published := RestartRecovery.successful_enqueue_is_actual_notification
      result.closed document binding.notification wake notificationBinding before.queue
      result.continuation hn
    have finalCoherent := ToolDelivery.publishWakeNotification_success_toolProjectionCoherent
      result.closed result.continuation.execution document notificationBinding binding.notification published
    simpa only [he] using finalCoherent
  | trans _ _ first second => exact second (first coherent)

end SessionComposition
end CanonicalOutput.Execution
