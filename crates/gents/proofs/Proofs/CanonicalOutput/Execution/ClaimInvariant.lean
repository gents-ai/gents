import Proofs.CanonicalOutput.Execution.SessionComposition

/-!
# Claim, queue, and retry coherence

The claim owner is the bridge between the physical request document and the
logical session queue entry.  This invariant says that an idle composed world
has no active queue entry, while a claimed world names exactly the physical
request, session, logical active entry, and retry owner in that world.
-/
namespace CanonicalOutput.Execution.SessionComposition

def ClaimCoherent (state : World) : Prop :=
  (state.claimed = none ∧ state.queue.active = none) ∨
  ∃ binding,
    state.claimed = some binding ∧
    binding.physicalRequest = state.requestId ∧
    binding.session = state.sessionId ∧
    state.queue.active = some binding.logicalRequest ∧
    state.retry.request = binding.physicalRequest

theorem claimCoherent_currentClaim {state : World} (h : ClaimCoherent state)
    (claimed : state.claimed.isSome = true) : currentClaim state = true := by
  rcases h with hidle | ⟨binding, hbinding, hphysical, hsession, hactive, hretry⟩
  · simp [hidle.1] at claimed
  · simp [currentClaim, hbinding, hphysical, hsession, hactive, hretry]

theorem idleClaimCoherent (state : World)
    (hclaimed : state.claimed = none) (hactive : state.queue.active = none) :
    ClaimCoherent state :=
  Or.inl ⟨hclaimed, hactive⟩

theorem activation_preserves_claimCoherent
    (before after : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activate before actor now activation scope budget deadline = some after) :
    ClaimCoherent after := by
  unfold activate at h
  unfold Handover.claimAndActivate at h
  dsimp only at h
  repeat' first | contradiction | split at h
  all_goals cases h
  all_goals simp [ClaimCoherent, initialRetry, Handover.freshRequestWorld] at *
  all_goals aesop

private theorem preserve_from_control
    {before after : World} (coherent : ClaimCoherent before)
    (hrequest : after.requestId = before.requestId)
    (hsession : after.sessionId = before.sessionId)
    (hclaimed : after.claimed = before.claimed)
    (hactive : after.queue.active = before.queue.active)
    (hretry : after.retry.request = before.retry.request) : ClaimCoherent after := by
  rcases coherent with hi | ⟨binding, hb, hp, hs, ha, hr⟩
  · exact Or.inl ⟨hclaimed.trans hi.1, hactive.trans hi.2⟩
  · exact Or.inr ⟨binding, hclaimed.trans hb, hp.trans hrequest.symm,
      hs.trans hsession.symm, hactive.trans ha, hretry.trans hr⟩

theorem finish_preserves_claimCoherent
    (before after : World) (actor : Gate.Actor)
    (acknowledged : List BackgroundCompletion.NotificationBinding)
    (h : finish before actor = some (after, acknowledged)) : ClaimCoherent after := by
  unfold finish Handover.finishAndAcknowledge at h
  dsimp only at h
  repeat' first | contradiction |
    (solve | cases h; exact Or.inl ⟨rfl,
      Handover.finishActive_step_clears_active _ _ (by assumption)⟩) |
    split at h

private theorem completion_step_preserves_request
    (before after : CompletionRetry.State) (action : CompletionRetry.Action)
    (h : CompletionRetry.step? before action = some after) :
    after.request = before.request := by
  cases action <;> simp [CompletionRetry.step?] at h <;>
    repeat' split at h <;> try contradiction
  all_goals aesop

private theorem canonical_policyStep_preserves_request
    (before after : CompletionRetry.State)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : CompletionRetry.CanonicalGate.policyStep before operation = .ok after) :
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
  all_goals apply Eq.trans (canonical_policyStep_preserves_request _ _ operation (by assumption))
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
      exact preserve_from_control coherent hi.1 hi.2 hc.1
        (congrArg SessionQueue.SessionQueueState.active hc.2)
        (provider_preserves_retry_request _ _ actor now operation h)
  | policy before view now operation claimed h =>
      have he := CompletionRetry.CanonicalGate.stepPolicy_preserves_gate now operation h
      exact preserve_from_control coherent
        (by simpa using congrArg World.requestId he)
        (by simpa using congrArg World.sessionId he)
        (by simpa using congrArg World.claimed he)
        (by simpa using congrArg (fun w => w.queue.active) he)
        (policy_preserves_retry_request before view now operation h)
  | gate before view actor now operation h =>
      unfold CompletionRetry.CanonicalGate.commitGate at h
      split at h <;> try contradiction
      have hc := Gate.commit_preserves_composed_control before view actor now operation.val h
      have hi := Gate.successful_commit_preserves_request_identity before view actor now operation.val h
      exact preserve_from_control coherent hi.1 hi.2 hc.2.1
        (congrArg SessionQueue.SessionQueueState.active hc.1) (congrArg CompletionRetry.State.request hc.2.2)
  | acquire before view actor independent h =>
      rw [Gate.acquire_preserves_durable_world before view actor independent h]
      exact coherent
  | scheduling before view actor event h =>
      rw [Gate.scheduling_preserves_durable_world before view actor event h]
      exact coherent
  | activate actor now activation scope budget deadline h =>
      exact activation_preserves_claimCoherent _ _ actor now activation scope budget deadline h
  | finish actor acknowledged h => exact finish_preserves_claimCoherent _ _ actor acknowledged h
  | activateGoal actor now result published routes authenticated generation duration leaseDeadline scope budget deadline h =>
      rename_i prior next
      unfold SessionComposition.activateGoal at h
      cases hc : Handover.claimAndActivate prior actor now
          (GoalContinuation.childActivation result routes authenticated generation duration leaseDeadline) with
      | none => simp [hc] at h
      | some session =>
          simp [hc] at h
          cases h
          unfold Handover.claimAndActivate at hc
          dsimp only at hc
          repeat' first | contradiction | split at hc
          all_goals cases hc
          all_goals simp [ClaimCoherent, initialRetry, Handover.freshRequestWorld] at *
          all_goals aesop
  | beginProcessing before after actor now generation h =>
      unfold Handover.beginProcessing at h
      dsimp only at h
      repeat' first | contradiction |
        (solve | cases h; simpa [ClaimCoherent] using coherent) |
        split at h
  | wake before after actor now document message entry binding h =>
      have hf := BackgroundGate.successful_commit_preserves_claim_control
        before after actor now document message entry binding h
      exact preserve_from_control coherent hf.1 hf.2.1 hf.2.2.1 hf.2.2.2.2
        (congrArg CompletionRetry.State.request hf.2.2.2.1)
  | goal before goal actor now request binding entry result h =>
      have hf := GoalContinuation.successful_publication_preserves_claim_control
        goal before actor now request binding entry result h
      exact preserve_from_control coherent hf.1 hf.2.1 hf.2.2.1 hf.2.2.2.2
        (congrArg CompletionRetry.State.request hf.2.2.2.1)
  | trans left right ihleft ihrigh => exact ihrigh (ihleft coherent)

end CanonicalOutput.Execution.SessionComposition
