import Proofs.CanonicalOutput.Execution.Handover
import Proofs.CanonicalOutput.Execution.GoalContinuation
import Proofs.CompletionRetry.CanonicalGate

/-!
One session trace over the existing owners. This module supplies wiring, not a
second implementation of their transitions. In particular there is no raw gate
trace constructor: provider closure decisions pass through the retry adapter.
-/
namespace CanonicalOutput.Execution.SessionComposition

private theorem begin_preserves_nextSeq
    (before after : Handover.State) (actor : Gate.Actor) (now : Time) (generation : Generation)
    (h : Handover.beginProcessing before actor now generation = some after) :
    after.paired.gate.execution.transcript.nextSeq = before.paired.gate.execution.transcript.nextSeq := by
  unfold Handover.beginProcessing at h
  dsimp only at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases h
  rfl

private theorem finish_preserves_nextSeq
    (before : Handover.State) (after : Handover.FinishResult) (actor : Gate.Actor)
    (h : Handover.finishAndAcknowledge before actor = some after) :
    after.state.paired.gate.execution.transcript.nextSeq = before.paired.gate.execution.transcript.nextSeq := by
  unfold Handover.finishAndAcknowledge at h
  dsimp only at h
  repeat' split at h <;> try contradiction
  all_goals cases h; rfl

structure State where
  session : Handover.State
  retry : CompletionRetry.State

def retryView (state : State) : CompletionRetry.CanonicalGate.State :=
  { gate := state.session.paired.gate, retry := state.retry }

def withRetryView (state : State) (view : CompletionRetry.CanonicalGate.State) : State :=
  { session := { state.session with paired :=
      { state.session.paired with gate := view.gate } }, retry := view.retry }

/-- The request binding is the claim owner's observation, not a cast between
logical queue IDs and physical output/request IDs. -/
def currentClaim (state : State) : Bool :=
  match state.session.claimed with
  | none => false
  | some binding =>
      binding.physicalRequest == state.session.paired.gate.execution.requestId &&
      binding.logicalRequest == state.session.paired.queue.active.getD 0 &&
      state.session.paired.queue.active.isSome &&
      binding.session == state.session.paired.gate.execution.sessionId &&
      state.retry.request == binding.physicalRequest

def commitProvider (state : State) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation) :
    Except CompletionRetry.CanonicalGate.Error State :=
  if !currentClaim state then .error .request
  else do
    let view ← CompletionRetry.CanonicalGate.commit (retryView state) actor now operation
    pure (withRetryView state view)

theorem provider_commit_preserves_claim_and_queue
    (before after : State) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : commitProvider before actor now operation = .ok after) :
    after.session.claimed = before.session.claimed ∧
    after.session.paired.queue = before.session.paired.queue := by
  unfold commitProvider at h
  split at h <;> try contradiction
  cases hc : CompletionRetry.CanonicalGate.commit (retryView before) actor now operation with
  | error error => simp [hc] at h
  | ok view => simp [hc] at h; cases h; exact ⟨rfl, rfl⟩

theorem provider_commit_is_actual_gate_commit
    (before after : State) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : commitProvider before actor now operation = .ok after) :
    Gate.commit before.session.paired.gate actor now
      (CompletionRetry.CanonicalGate.gateOperation operation) =
      some after.session.paired.gate := by
  unfold commitProvider at h
  split at h <;> try contradiction
  cases hc : CompletionRetry.CanonicalGate.commit (retryView before) actor now operation with
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

def activate (state : State) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time) : Option State := do
  if !nonGoalActivation activation then none
  let session ← Handover.claimAndActivate state.session actor now activation
  some { session := session, retry := initialRetry activation.request.document now scope budget deadline }

theorem goal_claim_requires_publication_route
    (state : State) (actor : Gate.Actor) (now : Time)
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

def activateGoal (state : State) (actor : Gate.Actor) (now : Time)
    (result : GoalContinuation.Result) (_published : GoalPublication result)
    (routes : List (DocId × Nat)) (routesAuthenticated : Bool)
    (generation : Generation) (duration leaseDeadline : Time) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time) : Option State := do
  let activation := GoalContinuation.childActivation result routes routesAuthenticated
    generation duration leaseDeadline
  let session ← Handover.claimAndActivate state.session actor now activation
  some { session := session, retry := initialRetry activation.request.document now scope budget deadline }

def finish (state : State) (actor : Gate.Actor) :
    Option (State × List BackgroundCompletion.NotificationBinding) := do
  let result ← Handover.finishAndAcknowledge state.session actor
  some ({ state with session := result.state }, result.acknowledged)

theorem activation_binds_retry_to_claim
    (before after : State) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activate before actor now activation scope budget deadline = some after) :
    after.retry.request = after.session.paired.gate.execution.requestId ∧
    after.retry.phase = .issuing ∧ after.retry.attempt = 0 := by
  unfold activate at h
  split at h <;> try contradiction
  cases hc : Handover.claimAndActivate before.session actor now activation with
  | none => simp [hc] at h
  | some session =>
      simp [hc] at h
      cases h
      have hb := Handover.successful_claim_has_exact_binding
        before.session session actor now activation hc
      exact ⟨hb.2.1.symm, rfl, rfl⟩

/-- A session history interleaves existing owners, not arbitrary replacement of
their records. Acquisition and non-provider tool work also remain available
between requests. Policy progress and provider commits require the exact claim. -/
inductive Trace : State → State → Prop where
  | refl (state : State) : Trace state state
  | provider {before after : State} (actor : Gate.Actor) (now : Time)
      (operation : CompletionRetry.CanonicalGate.Operation)
      (h : commitProvider before actor now operation = .ok after) : Trace before after
  | policy (before : State) (view : CompletionRetry.CanonicalGate.State) (now : Time)
      (operation : CompletionRetry.CanonicalGate.PolicyOperation)
      (claimed : currentClaim before = true)
      (h : CompletionRetry.CanonicalGate.stepPolicy (retryView before) now operation = some view) :
      Trace before (withRetryView before view)
  | gate (before : State) (view : CompletionRetry.CanonicalGate.State)
      (actor : Gate.Actor) (now : Time) (operation : CompletionRetry.CanonicalGate.GateOperation)
      (h : CompletionRetry.CanonicalGate.commitGate (retryView before) actor now operation = some view) :
      Trace before (withRetryView before view)
  | acquire (before : State) (view : CompletionRetry.CanonicalGate.State)
      (actor : Gate.Actor) (independent : Bool)
      (h : CompletionRetry.CanonicalGate.acquire (retryView before) actor independent = some view) :
      Trace before (withRetryView before view)
  | scheduling (before : State) (view : CompletionRetry.CanonicalGate.State)
      (actor : Gate.Actor) (event : StorageWriteGate.Event)
      (h : CompletionRetry.CanonicalGate.scheduling (retryView before) actor event = some view) :
      Trace before (withRetryView before view)
  | activate {before after : State} (actor : Gate.Actor) (now : Time)
      (activation : Handover.Activation) (scope : Nat)
      (budget : CompletionRetry.Budget) (deadline : Option Time)
      (h : activate before actor now activation scope budget deadline = some after) : Trace before after
  | finish {before after : State} (actor : Gate.Actor)
      (acknowledged : List BackgroundCompletion.NotificationBinding)
      (h : finish before actor = some (after, acknowledged)) : Trace before after
  | activateGoal {before after : State} (actor : Gate.Actor) (now : Time)
      (result : GoalContinuation.Result) (published : GoalPublication result)
      (routes : List (DocId × Nat)) (authenticated : Bool)
      (generation : Generation) (duration leaseDeadline : Time) (scope : Nat)
      (budget : CompletionRetry.Budget) (deadline : Option Time)
      (h : activateGoal before actor now result published routes authenticated generation
        duration leaseDeadline scope budget deadline = some after) : Trace before after
  | beginProcessing (before : State) (session : Handover.State)
      (actor : Gate.Actor) (now : Time) (generation : Generation)
      (h : Handover.beginProcessing before.session actor now generation = some session) :
      Trace before { before with session := session }
  | wake (before : State) (paired : BackgroundGate.State) (actor : Gate.Actor) (now : Time)
      (document : DocId) (message : MessageEnvelope) (entry : SessionQueue.QueueEntry)
      (binding : WakeDocumentBinding)
      (h : BackgroundGate.commit before.session.paired actor now document message entry binding = some paired) :
      Trace before { before with session := { before.session with paired := paired } }
  | goal (before : State) (goal : GoalAutomation.OperatorResume.Snapshot)
      (actor : Gate.Actor) (now : Time)
      (request : GoalAutomation.OperatorResume.ClaimedRequest)
      (binding : GoalContinuation.Binding) (entry : SessionQueue.QueueEntry)
      (result : GoalContinuation.Result)
      (h : GoalContinuation.publishGoalChild? goal before.session actor now request binding entry = some result) :
      Trace before { before with session := result.after }
  | trans {first second third : State} : Trace first second → Trace second third → Trace first third

theorem provider_commit_nextSequence_monotone
    (before after : State) (actor : Gate.Actor) (now : Time)
    (operation : CompletionRetry.CanonicalGate.Operation)
    (h : commitProvider before actor now operation = .ok after) :
    before.session.paired.gate.execution.transcript.nextSeq ≤
      after.session.paired.gate.execution.transcript.nextSeq := by
  unfold commitProvider at h
  split at h <;> try contradiction
  cases hc : CompletionRetry.CanonicalGate.commit (retryView before) actor now operation with
  | error error => simp [hc] at h
  | ok view =>
      simp [hc] at h
      cases h
      exact CompletionRetry.CanonicalGate.commit_nextSequence_monotone actor now operation hc

theorem activate_preserves_nextSequence
    (before after : State) (actor : Gate.Actor) (now : Time)
    (activation : Handover.Activation) (scope : Nat)
    (budget : CompletionRetry.Budget) (deadline : Option Time)
    (h : activate before actor now activation scope budget deadline = some after) :
    after.session.paired.gate.execution.transcript.nextSeq =
      before.session.paired.gate.execution.transcript.nextSeq := by
  unfold activate at h
  split at h <;> try contradiction
  cases hc : Handover.claimAndActivate before.session actor now activation with
  | none => simp [hc] at h
  | some session =>
      simp [hc] at h
      cases h
      exact Handover.successful_claim_preserves_nextSeq _ _ actor now activation hc

theorem finish_preserves_nextSequence
    (before after : State) (actor : Gate.Actor)
    (acknowledged : List BackgroundCompletion.NotificationBinding)
    (h : finish before actor = some (after, acknowledged)) :
    after.session.paired.gate.execution.transcript.nextSeq =
      before.session.paired.gate.execution.transcript.nextSeq := by
  unfold finish at h
  cases hc : Handover.finishAndAcknowledge before.session actor with
  | none => simp [hc] at h
  | some result =>
      simp [hc] at h
      rcases h with ⟨rfl, rfl⟩
      exact finish_preserves_nextSeq _ _ actor hc

theorem Trace.nextSequence_monotone {before after : State} (trace : Trace before after) :
    before.session.paired.gate.execution.transcript.nextSeq ≤
      after.session.paired.gate.execution.transcript.nextSeq := by
  induction trace with
  | refl => exact Nat.le_refl _
  | provider actor now operation h => exact provider_commit_nextSequence_monotone _ _ actor now operation h
  | policy before view now operation _ h =>
      have he := CompletionRetry.CanonicalGate.stepPolicy_preserves_gate now operation h
      simp [withRetryView, retryView, he]
  | gate before view actor now operation h =>
      exact CompletionRetry.CanonicalGate.commitGate_nextSequence_monotone actor now operation h
  | acquire before view actor independent h =>
      have he := CompletionRetry.CanonicalGate.acquire_preserves_gate_world actor independent h
      simp [withRetryView, retryView, he]
  | scheduling before view actor event h =>
      have he := CompletionRetry.CanonicalGate.scheduling_preserves_gate_world actor event h
      simp [withRetryView, retryView, he]
  | activate actor now activation scope budget deadline h =>
      rw [activate_preserves_nextSequence _ _ actor now activation scope budget deadline h]
  | finish actor acknowledged h => rw [finish_preserves_nextSequence _ _ actor acknowledged h]
  | activateGoal actor now result published routes authenticated generation duration leaseDeadline scope budget deadline h =>
      rename_i prior next
      unfold SessionComposition.activateGoal at h
      cases hc : Handover.claimAndActivate prior.session actor now
          (GoalContinuation.childActivation result routes authenticated generation duration leaseDeadline) with
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
        goal before.session actor now request binding entry result h).symm
  | trans _ _ first second => exact Nat.le_trans first second

end CanonicalOutput.Execution.SessionComposition
