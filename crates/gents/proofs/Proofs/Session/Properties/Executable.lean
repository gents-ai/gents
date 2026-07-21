import Proofs.Session.Executable

/-! Soundness and completeness of executable session queue actions. -/

namespace SessionQueue

theorem step?_sound
    {pre post : SessionQueueState}
    {action : Action}
    (h_step : step? pre action = some post) :
    Transition pre post := by
  cases action with
  | appendPending entry =>
      by_cases h_guard :
        entry.policy = .append ∧
          entry.appendWellFormed ∧
          RequestIdFresh pre entry ∧
          canAppendAfter pre.pending entry = true
      · simp [step?, h_guard] at h_step
        cases h_step
        exact Transition.append_pending h_guard.1 h_guard.2.1 h_guard.2.2.1 h_guard.2.2.2 rfl
      · simp [step?, h_guard] at h_step
  | coalescePending entry =>
      cases h_key : entry.queueKey with
      | none =>
          simp [step?, h_key] at h_step
      | some key =>
          by_cases h_guard : entry.coalesceWellFormed key
          · by_cases h_contains : containsCoalescedQueueKey pre.pending entry.source key = true
            · simp [step?, h_key, h_guard, h_contains] at h_step
              cases h_step
              exact Transition.coalesce_pending_existing h_guard h_contains rfl
            · have h_missing : containsCoalescedQueueKey pre.pending entry.source key = false := by
                cases h_value : containsCoalescedQueueKey pre.pending entry.source key
                · rfl
                · exact absurd h_value h_contains
              by_cases h_fresh_after :
                RequestIdFresh pre entry ∧ canAppendAfter pre.pending entry = true
              · simp [step?, h_key, h_guard, h_contains, h_fresh_after] at h_step
                cases h_step
                exact Transition.coalesce_pending_new h_guard h_fresh_after.1 h_missing h_fresh_after.2 rfl
              · simp [step?, h_key, h_guard, h_contains, h_fresh_after] at h_step
          · simp [step?, h_key, h_guard] at h_step
  | claimNext =>
      cases h_active : pre.active with
      | none =>
          cases h_pending : pre.pending with
          | nil =>
              simp [step?, h_active, h_pending] at h_step
          | cons entry rest =>
              simp [step?, h_active, h_pending] at h_step
              cases h_step
              exact Transition.claim_next h_active h_pending rfl
      | some requestId =>
          simp [step?, h_active] at h_step
  | finishActive =>
      cases h_active : pre.active with
      | none =>
          simp [step?, h_active] at h_step
      | some requestId =>
          simp [step?, h_active] at h_step
          cases h_step
          exact Transition.finish_active h_active rfl
  | drainAutomated source queueKey =>
      by_cases h_source : source.automatedWakeup
      · simp [step?, h_source] at h_step
        cases h_step
        exact Transition.drain_automated h_source rfl
      · simp [step?, h_source] at h_step

theorem transition_complete
    {pre post : SessionQueueState}
    (h_trans : Transition pre post) :
    ∃ action : Action, step? pre action = some post := by
  match h_trans with
  | Transition.append_pending (entry := entry) h_policy h_well_formed h_fresh h_after h_post =>
      exact ⟨.appendPending entry, by
        simp [step?, h_policy, h_well_formed, h_fresh, h_after, h_post]⟩
  | Transition.coalesce_pending_new (entry := entry) (key := key) h_well_formed h_fresh h_missing h_after h_post =>
      exact ⟨.coalescePending entry, by
        rcases h_well_formed with ⟨h_source, h_policy, h_key⟩
        have h_missing_source :
            containsCoalescedQueueKey pre.pending QueueSource.backgroundCompletion key = false := by
          simpa [h_source] using h_missing
        have h_fresh_after : RequestIdFresh pre entry ∧ canAppendAfter pre.pending entry = true :=
          ⟨h_fresh, h_after⟩
        simp [step?, QueueEntry.coalesceWellFormed, h_source, h_policy, h_key, h_missing,
          h_missing_source, h_fresh_after, h_post]⟩
  | Transition.coalesce_pending_existing (entry := entry) (key := key) h_well_formed h_contains h_post =>
      exact ⟨.coalescePending entry, by
        rcases h_well_formed with ⟨h_source, h_policy, h_key⟩
        have h_contains_source :
            containsCoalescedQueueKey pre.pending QueueSource.backgroundCompletion key = true := by
          simpa [h_source] using h_contains
        simp [step?, QueueEntry.coalesceWellFormed, h_source, h_policy, h_key, h_contains,
          h_contains_source, h_post]⟩
  | Transition.claim_next h_active h_pending h_post =>
      exact ⟨.claimNext, by simp [step?, h_active, h_pending, h_post]⟩
  | Transition.finish_active h_active h_post =>
      exact ⟨.finishActive, by simp [step?, h_active, h_post]⟩
  | Transition.drain_automated (source := source) (queueKey := queueKey) h_source h_post =>
      exact ⟨.drainAutomated source queueKey, by simp [step?, h_source, h_post]⟩

theorem replay?_sound
    {start finish : SessionQueueState}
    {actions : List Action}
    (h_replay : replay? start actions = some finish) :
    Trace start finish := by
  induction actions generalizing start with
  | nil =>
      simp [replay?] at h_replay
      cases h_replay
      exact Trace.refl
  | cons action rest ih =>
      simp [replay?] at h_replay
      cases h_step : step? start action with
      | none =>
          simp [h_step] at h_replay
      | some next =>
          simp [h_step] at h_replay
          exact Trace.step (step?_sound h_step) (ih h_replay)

theorem trace_complete
    {pre post : SessionQueueState}
    (h_trace : Trace pre post) :
    ∃ actions : List Action, replay? pre actions = some post := by
  induction h_trace with
  | refl =>
      exact ⟨[], rfl⟩
  | step h_trans _ ih =>
      rcases transition_complete h_trans with ⟨action, h_action⟩
      rcases ih with ⟨actions, h_actions⟩
      refine ⟨action :: actions, ?_⟩
      simp [replay?, h_action, h_actions]

end SessionQueue
