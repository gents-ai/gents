import Proofs.CanonicalOutput.Execution.SessionComposition

/-!
# Claim, queue, and retry coherence

The claim owner binds the physical request document to its retry owner. Normal
claims also own the active logical queue entry. Title claims use authenticated
parent provenance instead and do not modify the session queue.
-/
namespace CanonicalOutput.Execution.SessionComposition

def ClaimCoherent (state : World) : Prop :=
  (state.claimed = none ∧ (state.purpose = .normal → state.queue.active = none)) ∨
  ∃ binding,
    state.claimed = some binding ∧
    Handover.claimReady state binding = true ∧
    state.retry.request = binding.physicalRequest

theorem claimCoherent_currentClaim {state : World} (h : ClaimCoherent state)
    (claimed : state.claimed.isSome = true) : currentClaim state = true := by
  rcases h with hidle | ⟨binding, hbinding, hready, hretry⟩
  · simp [hidle.1] at claimed
  · simp [currentClaim, hbinding, hready, hretry]

theorem idleClaimCoherent (state : World)
    (hclaimed : state.claimed = none) (hactive : state.queue.active = none) :
    ClaimCoherent state :=
  Or.inl ⟨hclaimed, fun _ => hactive⟩

private theorem claimed_with_initialRetry_coherent
    (before claimed : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : Handover.claimAndActivate before actor now activation = some claimed) :
    ClaimCoherent
      { claimed with retry := initialRetry activation.request.document now scope budget deadline } := by
  unfold Handover.claimAndActivate at h
  dsimp only at h
  repeat' first | contradiction | split at h
  all_goals cases h
  all_goals simp [ClaimCoherent, initialRetry, Handover.freshRequestWorld] at *
  all_goals cases he : activation.evidence <;>
    simp_all [Handover.claimReady, Handover.evidenceValid, he]

theorem activation_preserves_claimCoherent
    (before after : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activate before actor now activation scope budget deadline = some after) :
    ClaimCoherent after := by
  unfold activate at h
  split at h <;> try contradiction
  cases hc : Handover.claimAndActivate before actor now activation with
  | none => simp [hc] at h
  | some claimed =>
      simp [hc] at h
      cases h
      exact claimed_with_initialRetry_coherent before claimed actor now activation scope budget
        deadline hc

theorem title_activation_preserves_claimCoherent
    (before after : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.TitleActivation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activateTitle before actor now activation scope budget deadline = some after) :
    ClaimCoherent after := by
  unfold activateTitle at h
  cases hc : Handover.claimTitle before actor now activation with
  | none => simp [hc] at h
  | some claimed =>
      simp [hc] at h
      cases h
      unfold Handover.claimTitle at hc
      dsimp only at hc
      repeat' first | contradiction | split at hc
      all_goals cases hc
      all_goals simp [ClaimCoherent, initialRetry, Handover.claimReady, Gate.atTime] at *
      all_goals aesop

private theorem preserve_from_control
    {before after : World} (coherent : ClaimCoherent before)
    (hrequest : after.requestId = before.requestId)
    (hsession : after.sessionId = before.sessionId)
    (hpurpose : after.purpose = before.purpose)
    (hprincipal : after.principal = before.principal)
    (hclaimed : after.claimed = before.claimed)
    (hactive : after.queue.active = before.queue.active)
    (hretry : after.retry.request = before.retry.request) : ClaimCoherent after := by
  rcases coherent with hi | ⟨binding, hb, hready, hr⟩
  · exact Or.inl ⟨hclaimed.trans hi.1, fun hp => hactive.trans (hi.2 (hpurpose.symm.trans hp))⟩
  · refine Or.inr ⟨binding, hclaimed.trans hb, ?_, hretry.trans hr⟩
    simpa [Handover.claimReady, hrequest, hsession, hpurpose, hprincipal, hactive]
      using hready

theorem finish_preserves_claimCoherent
    (before after : World) (actor : Gate.Actor)
    (acknowledged : List BackgroundCompletion.NotificationBinding)
    (h : finish before actor = some (after, acknowledged)) : ClaimCoherent after := by
  unfold finish at h
  cases hc : Handover.finishAndAcknowledge before actor with
  | none => simp [hc] at h
  | some result =>
      simp [hc] at h
      rcases h with ⟨rfl, rfl⟩
      have hf := Handover.successful_finish_clears_claim_control before result actor hc
      exact Or.inl ⟨hf.1, by
        intro hp
        have hpurpose : result.state.purpose = before.purpose := by
          rw [Handover.successful_finish_frame before result actor hc]
        exact hf.2.1 (hpurpose.symm.trans hp)⟩

private theorem completion_step_preserves_request
    (before after : CompletionRetry.State) (action : CompletionRetry.Action)
    (h : CompletionRetry.step? before action = some after) :
    after.request = before.request := by
  cases action <;> simp [CompletionRetry.step?] at h <;>
    repeat' split at h <;> try contradiction
  all_goals aesop

private theorem canonical_policyStep_preserves_request
    (purpose : RequestPurpose) (before after : CompletionRetry.State)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : CompletionRetry.CanonicalGate.policyStep purpose before operation = .ok after) :
    after.request = before.request := by
  cases operation <;> simp only [CompletionRetry.CanonicalGate.policyStep] at h <;>
    repeat' split at h <;> try contradiction
  all_goals try { cases h; rfl }
  all_goals cases h
  all_goals exact completion_step_preserves_request _ _ _ (by assumption)

private theorem retry_atTime_preserves_request
    (before after : CompletionRetry.State) (now : Time)
    (h : CompletionRetry.CanonicalGate.atTime before now = some after) :
    after.request = before.request := by
  unfold CompletionRetry.CanonicalGate.atTime at h
  split at h <;> try contradiction
  cases h
  rfl

private theorem provider_preserves_retry_request
    (before after : World) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : commitProvider before actor now operation = .ok after) :
    after.retry.request = before.retry.request := by
  unfold commitProvider CompletionRetry.CanonicalGate.commit at h
  repeat' first | contradiction | split at h
  all_goals cases h
  all_goals apply Eq.trans (canonical_policyStep_preserves_request before.purpose _ _ operation (by assumption))
  all_goals exact retry_atTime_preserves_request _ _ now (by assumption)

private theorem policy_preserves_retry_request
    (before after : World) (now : Time)
    (operation : CompletionRetry.CanonicalGate.PolicyOperation)
    (h : CompletionRetry.CanonicalGate.stepPolicy before now operation = some after) :
    after.retry.request = before.retry.request := by
  unfold CompletionRetry.CanonicalGate.stepPolicy at h
  split at h <;> try contradiction
  cases ht : CompletionRetry.CanonicalGate.atTime before.retry now with
  | none => simp [ht] at h
  | some observed =>
      simp [ht] at h
      cases hs : CompletionRetry.step? observed operation.val with
      | none => simp [hs] at h
      | some retry =>
          simp [hs] at h
          cases h
          apply Eq.trans (completion_step_preserves_request observed retry operation.val hs)
          unfold CompletionRetry.CanonicalGate.atTime at ht
          split at ht <;> try contradiction
          cases ht
          rfl

private theorem fold_preserves_purpose_principal {α : Type}
    (step : World → α → World)
    (hstep : ∀ world item,
      (step world item).purpose = world.purpose ∧
      (step world item).principal = world.principal)
    (items : List α) (world : World) :
    (items.foldl step world).purpose = world.purpose ∧
    (items.foldl step world).principal = world.principal := by
  induction items generalizing world with
  | nil => exact ⟨rfl, rfl⟩
  | cons item rest ih =>
      have hs := hstep world item
      have hr := ih (step world item)
      exact ⟨hr.1.trans hs.1, hr.2.trans hs.2⟩

private theorem accountOwnedTools_preserves_purpose_principal
    (world : World) (generation : Generation) (interruptRunning : Bool) :
    (accountOwnedTools world generation interruptRunning).purpose = world.purpose ∧
    (accountOwnedTools world generation interruptRunning).principal = world.principal := by
  unfold accountOwnedTools
  apply fold_preserves_purpose_principal
  intro current original
  exact ⟨rfl, rfl⟩

private theorem accountMetadataOwnedTools_preserves_purpose_principal
    (world : World) (generation : Generation) :
    (accountMetadataOwnedTools world generation).purpose = world.purpose ∧
    (accountMetadataOwnedTools world generation).principal = world.principal := by
  unfold accountMetadataOwnedTools
  apply fold_preserves_purpose_principal
  intro current original
  exact ⟨rfl, rfl⟩

private theorem preparedRecoveryWorld_preserves_purpose_principal
    (world : World) (prepared : RecoveryPrepared) (expected : Generation) :
    (preparedRecoveryWorld world prepared expected).purpose = world.purpose ∧
    (preparedRecoveryWorld world prepared expected).principal = world.principal := by
  unfold preparedRecoveryWorld
  exact accountOwnedTools_preserves_purpose_principal _ expected true

private theorem evaluate_preserves_purpose_principal
    (operation : Gate.Operation) (before after : World)
    (h : Gate.evaluate operation before = .ok after) :
    after.purpose = before.purpose ∧ after.principal = before.principal := by
  have hc := Gate.evaluate_success_core operation before after h
  cases operation <;> simp only [Gate.evaluateCore] at hc
  all_goals first
    | exact ToolDelivery.tool_write_preserves_purpose_principal
        (mapError_success Gate.Error.delivery _ _ hc)
    | skip
  case compact cursor =>
    cases hcompact : Compaction.advanceCursor? before cursor with
    | none => simp [hcompact] at hc
    | some post =>
        simp [hcompact] at hc
        subst after
        unfold Compaction.advanceCursor? at hcompact
        repeat' first | contradiction | (solve | cases hcompact; exact ⟨rfl, rfl⟩) | split at hcompact
  case closeAuxiliary generation closing =>
    rcases closeAuxiliary_success_effect before after generation closing
      (mapError_success Gate.Error.execution _ _ hc) with rfl | ⟨_, rfl⟩ <;>
      exact ⟨rfl, rfl⟩
  case accept generation closing message admissions =>
    have hcore := mapError_success Gate.Error.execution _ _ hc
    replace hcore := checked_core_success _ _ _ hcore
    rcases acceptAndPublishCore_success_effect before after generation closing message
      admissions hcore with ⟨rfl, _⟩ | ⟨_, _, rfl, _⟩ <;> exact ⟨rfl, rfl⟩
  case toolComplete document authority record message =>
    have hcomposed := mapError_success Gate.Error.delivery _ _ hc
    obtain ⟨closed, hclose, hdeliver⟩ := ToolDelivery.completeAndDeliver_success
      before after document authority record message hcomposed
    have hfirst := ToolDelivery.tool_write_preserves_purpose_principal hclose
    have hsecond := ToolDelivery.tool_write_preserves_purpose_principal hdeliver
    exact ⟨hsecond.1.trans hfirst.1, hsecond.2.trans hfirst.2⟩
  all_goals
    have hcore := mapError_success Gate.Error.execution _ _ hc
    try replace hcore := checked_core_success _ _ _ hcore
    simp only [Execution.renew, renewCore, appendRaw, appendRawCore,
      retractBeforeRetryCore, publishAuthoredCore, publishHeaderOnlyCore,
      dispatchCore, admitSpawnedBackgroundCore, changeToolControlCore,
      closePartialAndPublishCore, recoverExpiredBatchCore,
      recoverExpiredTerminalCore, terminalizeCore, revokeCorruptCore] at hcore
    try dsimp only at hcore
    repeat' first
      | contradiction
      | (solve | cases hcore; exact ⟨rfl, rfl⟩)
      | split at hcore
  all_goals
    cases hcore
    first
      | exact preparedRecoveryWorld_preserves_purpose_principal _ _ _
      | exact accountMetadataOwnedTools_preserves_purpose_principal _ _
      | exact accountOwnedTools_preserves_purpose_principal _ _ _

private theorem commit_preserves_purpose_principal
    (before after : World) (actor : Gate.Actor) (now : Time)
    (operation : Gate.Operation)
    (h : Gate.commit before actor now operation = some after) :
    after.purpose = before.purpose ∧ after.principal = before.principal := by
  obtain ⟨execution, he, rfl⟩ := Gate.commit_reads_current_world before after actor now operation h
  exact evaluate_preserves_purpose_principal operation (Gate.atTime before now) execution he

private theorem background_preserves_purpose_principal
    (before after : World) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope)
    (wake : SessionQueue.QueueEntry) (binding : WakeDocumentBinding)
    (h : BackgroundGate.commit before actor now document message wake binding = some after) :
    after.purpose = before.purpose ∧ after.principal = before.principal := by
  unfold BackgroundGate.commit at h
  split at h <;> try contradiction
  cases hp : BackgroundContinuation.publishAndEnqueue?
      (Gate.atTime before now) document message wake binding before.queue with
  | none => simp [hp] at h
  | some result =>
      simp [hp] at h
      rcases h with ⟨⟨⟨⟨hbefore, _⟩, _⟩, _⟩, rfl⟩
      have hf := ToolDelivery.wake_notification_preserves_purpose_principal
        result.before result.execution result.document result.binding result.message result.published
      exact ⟨by simpa [hbefore] using hf.1, by simpa [hbefore] using hf.2⟩

private theorem goal_preserves_purpose_principal
    (goal : GoalAutomation.OperatorResume.Snapshot) (before : World) (actor : Gate.Actor)
    (now : Time) (request : GoalAutomation.OperatorResume.ClaimedRequest)
    (binding : GoalContinuation.Binding) (entry : SessionQueue.QueueEntry)
    (result : GoalContinuation.Result)
    (h : GoalContinuation.publishGoalChild? goal before actor now request binding entry = some result) :
    result.after.purpose = before.purpose ∧ result.after.principal = before.principal := by
  unfold GoalContinuation.publishGoalChild? at h
  dsimp only at h
  repeat' first | contradiction |
    (solve | cases h; simp_all [GoalContinuation.releaseGate,
      SessionQueue.step?, GoalContinuation.actualSessionIdle]) |
    split at h
  cases h
  exact ⟨rfl, rfl⟩

private theorem restart_preserves_purpose_principal
    (before after : World) (actor : Gate.Actor) (now : Time) (document : DocId)
    (binding : RestartRecovery.RestartBinding) (closing : Segment)
    (wake : SessionQueue.QueueEntry) (notificationBinding : WakeDocumentBinding)
    (h : RestartRecovery.commit before actor now document binding closing wake notificationBinding = some after) :
    after.purpose = before.purpose ∧ after.principal = before.principal := by
  obtain ⟨result, _, hc, hn, he, hafter⟩ :=
    RestartRecovery.successful_commit_effect _ _ _ _ _ _ _ _ _ h
  subst after
  have hpublished := RestartRecovery.successful_enqueue_is_actual_notification
    result.closed document binding.notification wake notificationBinding before.queue
      result.continuation hn
  rw [he] at hpublished
  have hclose := ToolDelivery.tool_write_preserves_purpose_principal hc
  have hnotify := ToolDelivery.wake_notification_preserves_purpose_principal
    result.closed result.execution document notificationBinding binding.notification hpublished
  exact ⟨hnotify.1.trans hclose.1, hnotify.2.trans hclose.2⟩

theorem Trace.claimCoherent {before after : World}
    (trace : Trace before after) (coherent : ClaimCoherent before) :
    ClaimCoherent after := by
  induction trace with
  | refl => exact coherent
  | provider actor now operation h =>
      have hc := provider_commit_preserves_claim_and_queue _ _ actor now operation h
      have hi := Gate.successful_commit_preserves_request_identity _ _ actor now
        (CompletionRetry.CanonicalGate.gateOperation operation)
        (provider_commit_is_actual_gate_commit _ _ actor now operation h)
      have hp := commit_preserves_purpose_principal _ _ actor now
        (CompletionRetry.CanonicalGate.gateOperation operation)
        (provider_commit_is_actual_gate_commit _ _ actor now operation h)
      exact preserve_from_control coherent hi.1 hi.2 hp.1 hp.2 hc.1
        (congrArg SessionQueue.SessionQueueState.active hc.2)
        (provider_preserves_retry_request _ _ actor now operation h)
  | policy before view now operation claimed h =>
      have he := CompletionRetry.CanonicalGate.stepPolicy_preserves_gate now operation h
      exact preserve_from_control coherent
        (by simpa using congrArg World.requestId he)
        (by simpa using congrArg World.sessionId he)
        (by simpa using congrArg World.purpose he)
        (by simpa using congrArg World.principal he)
        (by simpa using congrArg World.claimed he)
        (by simpa using congrArg (fun w => w.queue.active) he)
        (policy_preserves_retry_request before view now operation h)
  | gate before view actor now operation h =>
      unfold CompletionRetry.CanonicalGate.commitGate at h
      split at h <;> try contradiction
      have hc := Gate.commit_preserves_composed_control before view actor now operation.val h
      have hi := Gate.successful_commit_preserves_request_identity before view actor now operation.val h
      have hp := commit_preserves_purpose_principal before view actor now operation.val h
      exact preserve_from_control coherent hi.1 hi.2 hp.1 hp.2 hc.2.1
        (congrArg SessionQueue.SessionQueueState.active hc.1) (congrArg CompletionRetry.State.request hc.2.2)
  | acquire before view actor independent h =>
      rw [Gate.acquire_preserves_durable_world before view actor independent h]
      exact coherent
  | scheduling before view actor event h =>
      rw [Gate.scheduling_preserves_durable_world before view actor event h]
      exact coherent
  | activate actor now activation scope budget deadline h =>
      exact activation_preserves_claimCoherent _ _ actor now activation scope budget deadline h
  | activateTitle actor now activation scope budget deadline h =>
      exact title_activation_preserves_claimCoherent _ _ actor now activation scope budget deadline h
  | finish actor acknowledged h => exact finish_preserves_claimCoherent _ _ actor acknowledged h
  | activateGoal actor now result published generation duration leaseDeadline scope budget deadline h =>
      rename_i prior next
      unfold SessionComposition.activateGoal at h
      cases hc : Handover.claimAndActivate prior actor now
          (GoalContinuation.childActivation result generation duration leaseDeadline) with
      | none => simp [hc] at h
      | some session =>
          simp [hc] at h
          cases h
          exact claimed_with_initialRetry_coherent prior session actor now
            (GoalContinuation.childActivation result generation duration
              leaseDeadline) scope budget deadline hc
  | beginProcessing before after actor now generation h =>
      rw [Handover.successful_begin_frame before after actor now generation h]
      exact coherent
  | wake before after actor now document message entry binding h =>
      have hf := BackgroundGate.successful_commit_preserves_claim_control
        before after actor now document message entry binding h
      have hp := background_preserves_purpose_principal
        before after actor now document message entry binding h
      exact preserve_from_control coherent hf.1 hf.2.1 hp.1 hp.2 hf.2.2.1 hf.2.2.2.2
        (congrArg CompletionRetry.State.request hf.2.2.2.1)
  | goal before goal actor now request binding entry result h =>
      have hf := GoalContinuation.successful_publication_preserves_claim_control
        goal before actor now request binding entry result h
      have hp := goal_preserves_purpose_principal
        goal before actor now request binding entry result h
      exact preserve_from_control coherent hf.1 hf.2.1 hp.1 hp.2 hf.2.2.1 hf.2.2.2.2
        (congrArg CompletionRetry.State.request hf.2.2.2.1)
  | restart before after actor now document binding closing wake notificationBinding h =>
      have hf := RestartRecovery.successful_commit_preserves_claim_control
        before after actor now document binding closing wake notificationBinding h
      have hp := restart_preserves_purpose_principal
        before after actor now document binding closing wake notificationBinding h
      exact preserve_from_control coherent hf.1 hf.2.1 hp.1 hp.2 hf.2.2.1 hf.2.2.2.2
        (congrArg CompletionRetry.State.request hf.2.2.2.1)
  | trans left right ihleft ihrigh => exact ihrigh (ihleft coherent)

end CanonicalOutput.Execution.SessionComposition
