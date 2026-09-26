import Proofs.CanonicalOutput.Execution.Handover
import Proofs.CanonicalOutput.Execution.GoalContinuation
import Proofs.CanonicalOutput.Execution.RestartRecovery
import Proofs.CompletionRetry.CanonicalGate

/-!
One session trace over the existing owners. This module supplies wiring, not a
second implementation of their transitions. In particular there is no raw gate
trace constructor: provider closure decisions pass through the retry adapter.
-/
namespace CanonicalOutput.Execution.SessionComposition

private theorem begin_preserves_nextSeq
    (before after : World) (actor : Gate.Actor) (now : Time) (generation : Generation)
    (h : Handover.beginProcessing before actor now generation = some after) :
    after.transcript.nextSeq = before.transcript.nextSeq := by
  rw [Handover.successful_begin_frame before after actor now generation h]

private theorem finish_preserves_nextSeq
    (before : World) (after : Handover.FinishResult) (actor : Gate.Actor)
    (h : Handover.finishAndAcknowledge before actor = some after) :
    after.state.transcript.nextSeq = before.transcript.nextSeq := by
  rw [Handover.successful_finish_frame before after actor h]

/-- The request binding is the claim owner's observation, not a cast between
logical queue IDs and physical output/request IDs. -/
def currentClaim (state : World) : Bool :=
  match state.claimed with
  | none => false
  | some binding =>
      Handover.claimReady state binding &&
        state.retry.request == binding.physicalRequest

def commitProvider (state : World) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation) :
    Except CompletionRetry.CanonicalGate.Error World :=
  if !currentClaim state then .error .request
  else do
    let view ← CompletionRetry.CanonicalGate.commit (state) actor now operation
    pure (view)

theorem provider_commit_preserves_claim_and_queue
    (before after : World) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : commitProvider before actor now operation = .ok after) :
    after.claimed = before.claimed ∧
    after.queue = before.queue := by
  unfold commitProvider at h
  split at h <;> try contradiction
  cases hc : CompletionRetry.CanonicalGate.commit (before) actor now operation with
  | error error => simp [hc] at h
  | ok view =>
      have hg := CompletionRetry.CanonicalGate.successful_commit_is_actual_gate_commit
        actor now operation hc
      have hf := Gate.commit_preserves_composed_control before
        { view with retry := before.retry } actor now
        (CompletionRetry.CanonicalGate.gateOperation operation) hg
      simp [hc] at h
      cases h
      exact ⟨hf.2.1, hf.1⟩

theorem provider_commit_is_actual_gate_commit
    (before after : World) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : commitProvider before actor now operation = .ok after) :
    Gate.commit before actor now
      (CompletionRetry.CanonicalGate.gateOperation operation) =
      some { after with retry := before.retry } := by
  unfold commitProvider at h
  split at h <;> try contradiction
  cases hc : CompletionRetry.CanonicalGate.commit (before) actor now operation with
  | error error => simp [hc] at h
  | ok view =>
      simp [hc] at h
      cases h
      exact CompletionRetry.CanonicalGate.successful_commit_is_actual_gate_commit actor now operation hc

/-- Retry initialization belongs to a newly claimed physical request. It cannot
carry accepted/retracted state or spent repair state from the previous request. -/
def initialRetry (request : DocId) (now : Time) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time) : CompletionRetry.State :=
  { request := request, scope := scope, turn := 0, phase := .issuing
  , budget := budget, transportUsed := 0, resampleUsed := 0, repairUsed := false
  , lastParseError := none, now := now, deadline := deadline, attempt := 0
  , usageCharged := 0 }

def nonGoalActivation (activation : Handover.Activation) : Bool :=
  match activation.evidence with
  | .goalChild _ => false
  | _ => true

def activate (state : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time) : Option World := do
  if !nonGoalActivation activation then none
  let world ← Handover.claimAndActivate state actor now activation
  some { world with retry := initialRetry activation.request.document now scope budget deadline }

/-- Direct title activation is a claim of its own signed request, not a
synthetic session-queue entry. The parent binding survives as claim evidence
for audit usage, but cannot gate this lease. -/
def activateTitle (state : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.TitleActivation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time) : Option World := do
  let world ← Handover.claimTitle state actor now activation
  some { world with retry := initialRetry activation.binding.physicalRequest now scope budget deadline }

theorem goal_claim_requires_publication_route
    (state : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (receipt : Handover.GoalChildReceipt)
    (scope : Nat) (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activation.evidence = .goalChild receipt) :
    activate state actor now activation scope budget deadline = none := by
  simp [activate, nonGoalActivation, h]

/-- Proof of an actual existing Goal-owner publication, not a caller-created
record with the right field names. This is trace evidence, not another durable
receipt or lifecycle. -/
def GoalPublication (result : GoalContinuation.Result) : Prop :=
  ∃ goal state actor now request binding entry,
    GoalContinuation.publishGoalChild? goal state actor now request binding entry = some result

def activateGoal (state : World) (actor : Gate.Actor) (now : Time)
    (result : GoalContinuation.Result) (_published : GoalPublication result)
    (generation : Generation) (duration leaseDeadline : Time) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time) : Option World := do
  let activation := GoalContinuation.childActivation result
    generation duration leaseDeadline
  let world ← Handover.claimAndActivate state actor now activation
  some { world with retry := initialRetry activation.request.document now scope budget deadline }

def finish (state : World) (actor : Gate.Actor) :
    Option (World × List BackgroundCompletion.NotificationBinding) := do
  let result ← Handover.finishAndAcknowledge state actor
  some (result.state, result.acknowledged)

theorem activation_binds_retry_to_claim
    (before after : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activate before actor now activation scope budget deadline = some after) :
    after.retry.request = after.requestId ∧
    after.retry.phase = .issuing ∧ after.retry.attempt = 0 := by
  unfold activate at h
  split at h <;> try contradiction
  cases hc : Handover.claimAndActivate before actor now activation with
  | none => simp [hc] at h
  | some session =>
      simp [hc] at h
      cases h
      have hb := Handover.successful_claim_has_exact_binding
        before session actor now activation hc
      exact ⟨hb.2.1.symm, rfl, rfl⟩

/-- A session history interleaves existing owners, not arbitrary replacement of
their records. Acquisition and non-provider tool work also remain available
between requests. Policy progress and provider commits require the exact claim. -/
inductive Trace : World → World → Prop where
  | refl (state : World) : Trace state state
  | provider {before after : World} (actor : Gate.Actor) (now : Time)
      (operation : CompletionRetry.CanonicalGate.Operation)
      (h : commitProvider before actor now operation = .ok after) : Trace before after
  | policy (before : World) (view : World) (now : Time)
      (operation : CompletionRetry.CanonicalGate.PolicyOperation)
      (claimed : currentClaim before = true)
      (h : CompletionRetry.CanonicalGate.stepPolicy (before) now operation = some view) :
      Trace before (view)
  | gate (before : World) (view : World)
      (actor : Gate.Actor) (now : Time) (operation : CompletionRetry.CanonicalGate.GateOperation)
      (h : CompletionRetry.CanonicalGate.commitGate (before) actor now operation = some view) :
      Trace before (view)
  | acquire (before : World) (view : World)
      (actor : Gate.Actor) (independent : Bool)
      (h : Gate.acquire before actor independent = some view) :
      Trace before (view)
  | scheduling (before : World) (view : World)
      (actor : Gate.Actor) (event : StorageWriteGate.Event)
      (h : Gate.scheduling before actor event = some view) :
      Trace before (view)
  | activate {before after : World} (actor : Gate.Actor) (now : Time)
      (activation : Handover.Activation) (scope : Nat)
      (budget : CompletionRetry.Budget) (deadline : Option Time)
      (h : activate before actor now activation scope budget deadline = some after) : Trace before after
  | activateTitle {before after : World} (actor : Gate.Actor) (now : Time)
      (activation : Handover.TitleActivation) (scope : Nat)
      (budget : CompletionRetry.Budget) (deadline : Option Time)
      (h : activateTitle before actor now activation scope budget deadline = some after) :
      Trace before after
  | finish {before after : World} (actor : Gate.Actor)
      (acknowledged : List BackgroundCompletion.NotificationBinding)
      (h : finish before actor = some (after, acknowledged)) : Trace before after
  | activateGoal {before after : World} (actor : Gate.Actor) (now : Time)
      (result : GoalContinuation.Result) (published : GoalPublication result)
      (generation : Generation) (duration leaseDeadline : Time) (scope : Nat)
      (budget : CompletionRetry.Budget) (deadline : Option Time)
      (h : activateGoal before actor now result published generation
        duration leaseDeadline scope budget deadline = some after) : Trace before after
  | beginProcessing (before after : World)
      (actor : Gate.Actor) (now : Time) (generation : Generation)
      (h : Handover.beginProcessing before actor now generation = some after) :
      Trace before after
  | wake (before after : World) (actor : Gate.Actor) (now : Time)
      (document : DocId) (message : MessageEnvelope) (entry : SessionQueue.QueueEntry)
      (binding : WakeDocumentBinding)
      (h : BackgroundGate.commit before actor now document message entry binding = some after) :
      Trace before after
  | goal (before : World) (goal : GoalAutomation.OperatorResume.Snapshot)
      (actor : Gate.Actor) (now : Time)
      (request : GoalAutomation.OperatorResume.ClaimedRequest)
      (binding : GoalContinuation.Binding) (entry : SessionQueue.QueueEntry)
      (result : GoalContinuation.Result)
      (h : GoalContinuation.publishGoalChild? goal before actor now request binding entry = some result) :
      Trace before result.after
  | restart (before after : World) (actor : Gate.Actor) (now : Time)
      (document : DocId) (binding : RestartRecovery.RestartBinding)
      (closing : Segment) (wake : SessionQueue.QueueEntry)
      (notificationBinding : WakeDocumentBinding)
      (h : RestartRecovery.commit before actor now document binding closing wake
        notificationBinding = some after) : Trace before after
  | trans {first second third : World} : Trace first second → Trace second third → Trace first third

theorem provider_commit_nextSequence_monotone
    (before after : World) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : commitProvider before actor now operation = .ok after) :
    before.transcript.nextSeq ≤
      after.transcript.nextSeq := by
  unfold commitProvider at h
  split at h <;> try contradiction
  cases hc : CompletionRetry.CanonicalGate.commit (before) actor now operation with
  | error error => simp [hc] at h
  | ok view =>
      simp [hc] at h
      cases h
      exact CompletionRetry.CanonicalGate.commit_nextSequence_monotone actor now operation hc

theorem activate_preserves_nextSequence
    (before after : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activate before actor now activation scope budget deadline = some after) :
    after.transcript.nextSeq =
      before.transcript.nextSeq := by
  unfold activate at h
  split at h <;> try contradiction
  cases hc : Handover.claimAndActivate before actor now activation with
  | none => simp [hc] at h
  | some session =>
      simp [hc] at h
      cases h
      exact Handover.successful_claim_preserves_nextSeq before session actor now activation hc

theorem activateTitle_preserves_nextSequence
    (before after : World) (actor : Gate.Actor) (now : Time)
    (activation : Handover.TitleActivation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activateTitle before actor now activation scope budget deadline = some after) :
    after.transcript.nextSeq = before.transcript.nextSeq := by
  unfold activateTitle at h
  cases hc : Handover.claimTitle before actor now activation with
  | none => simp [hc] at h
  | some claimed =>
      simp [hc] at h
      cases h
      rw [Handover.successful_title_claim_frame before claimed actor now activation hc]

theorem finish_preserves_nextSequence
    (before after : World) (actor : Gate.Actor)
    (acknowledged : List BackgroundCompletion.NotificationBinding)
    (h : finish before actor = some (after, acknowledged)) :
    after.transcript.nextSeq =
      before.transcript.nextSeq := by
  unfold finish at h
  cases hc : Handover.finishAndAcknowledge before actor with
  | none => simp [hc] at h
  | some result =>
      simp [hc] at h
      rcases h with ⟨rfl, rfl⟩
      exact finish_preserves_nextSeq _ _ actor hc

theorem Trace.nextSequence_monotone {before after : World} (trace : Trace before after) :
    before.transcript.nextSeq ≤
      after.transcript.nextSeq := by
  induction trace with
  | refl => exact Nat.le_refl _
  | provider actor now operation h => exact provider_commit_nextSequence_monotone _ _ actor now operation h
  | policy before view now operation _ h =>
      have he := CompletionRetry.CanonicalGate.stepPolicy_preserves_gate now operation h
      have hs := congrArg (fun world : World => world.transcript.nextSeq) he
      simpa using Nat.le_of_eq hs.symm
  | gate before view actor now operation h =>
      exact CompletionRetry.CanonicalGate.commitGate_nextSequence_monotone actor now operation h
  | acquire before view actor independent h =>
      have he := Gate.acquire_preserves_durable_world before view actor independent h
      have hs := congrArg (fun world : World => world.transcript.nextSeq) he
      simpa using Nat.le_of_eq hs.symm
  | scheduling before view actor event h =>
      have he := Gate.scheduling_preserves_durable_world before view actor event h
      have hs := congrArg (fun world : World => world.transcript.nextSeq) he
      simpa using Nat.le_of_eq hs.symm
  | activate actor now activation scope budget deadline h =>
      rw [activate_preserves_nextSequence _ _ actor now activation scope budget deadline h]
  | activateTitle actor now activation scope budget deadline h =>
      rw [activateTitle_preserves_nextSequence _ _ actor now activation scope budget deadline h]
  | finish actor acknowledged h => rw [finish_preserves_nextSequence _ _ actor acknowledged h]
  | activateGoal actor now result published generation duration leaseDeadline scope budget deadline h =>
      rename_i prior next
      unfold SessionComposition.activateGoal at h
      cases hc : Handover.claimAndActivate prior actor now
          (GoalContinuation.childActivation result generation duration leaseDeadline) with
      | none => simp [hc] at h
      | some session =>
          simp [hc] at h
          cases h
          exact Nat.le_of_eq (Handover.successful_claim_preserves_nextSeq _ _ actor now _ hc).symm
  | beginProcessing before session actor now generation h =>
      exact Nat.le_of_eq (begin_preserves_nextSeq _ _ actor now generation h).symm
  | wake before paired actor now document message entry binding h =>
      exact BackgroundGate.successful_commit_preserves_allocator_monotonicity
        _ _ actor now document message entry binding h
  | goal before goal actor now request binding entry result h =>
      exact Nat.le_of_eq (GoalContinuation.successful_publish_preserves_nextSeq
        goal before actor now request binding entry result h).symm
  | restart before after actor now document binding closing wake notificationBinding h =>
      exact RestartRecovery.successful_commit_nextSequence_monotone before after actor now
        document binding closing wake notificationBinding h
  | trans _ _ first second => exact Nat.le_trans first second

end CanonicalOutput.Execution.SessionComposition
