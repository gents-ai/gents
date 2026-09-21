import Proofs.CanonicalOutput.Execution.InvariantComposition
import Proofs.CanonicalOutput.Execution.ClaimInvariant
import Proofs.CanonicalOutput.Execution.ClosureInvariant
import Proofs.CanonicalOutput.Execution.CoherenceComposition
import Proofs.CanonicalOutput.Execution.GoalContinuationCases
import Proofs.CanonicalOutput.Execution.Examples

namespace CanonicalOutput.Execution.SessionComposition.Cases

open CanonicalOutput
open CanonicalOutput.Execution.GoalContinuation.Cases

def budget : CompletionRetry.Budget :=
  { transportRetries := 2, resampleRetries := 1, allowRepair := true }

def ordinaryEntry : SessionQueue.QueueEntry :=
  { requestId := 20, createdAt := 6, source := .user, policy := .append
  , queueKey := none, queuedAfter := some 10 }

def ordinaryActivation : Handover.Activation :=
  { request :=
      { document := 200, entry := ordinaryEntry, agent := 1, session := 1
      , requester := none, authenticated := true }
  , evidence := .ordinary, configuredRoutes := [], routesAuthenticated := true
  , generation := 8, duration := 5, deadline := 11 }

def initial : Option World := do
  let gate ← Gate.acquire (Gate.initial parentWorld) 1 true
  let queue : SessionQueue.SessionQueueState :=
    { scope := ⟨1, 1, none⟩, active := none, pending := [ordinaryEntry], terminal := ∅ }
  pure { gate with
    queue := queue
    claimed := none
    retry := initialRetry 999 5 0 budget (some 11) }

def releaseAcquire (state : World) : Option World := do
  let released ← Gate.scheduling state 1 .release
  let held ← Gate.acquire released 1 true
  pure held

def providerRaw : Segment :=
  { id := 810, coordinate := ⟨200, .provider 0 0 0⟩, writer := .request 8
  , flush := some ⟨0, [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩], [65]⟩
  , close := none, createdAt := 6 }

def retryClose : Segment :=
  { id := 811, coordinate := ⟨200, .provider 0 0 0⟩, writer := .request 8
  , flush := none, close := some .retracted, createdAt := 6 }

def acceptedClose : Segment :=
  { id := 812, coordinate := ⟨200, .provider 0 0 1⟩, writer := .request 8
  , flush := none, close := some (.closed .complete 0 []), createdAt := 9 }

def acceptedMessage : MessageEnvelope :=
  { header :=
      { id := 813, session := 1, request := some 200, origin := none
      , refs := [], outcome := .complete, role := .assistant
      , publication := .requestExecution 8 }
  , key := "assistant-813", sequence := 0, nativeId := none, blocks := []
  , createdAt := 9 }

/-- One executable application trace crosses the real queue/claim owner, lease
owner, retry policy, canonical transcript gate, terminal owner, and queue finish
owner. Physical request `200` remains distinct from logical queue request `20`.
No state record is replaced between transitions; only the owners' acquire/release
operations move the storage gate. -/
@[noinline] opaque begunPhase : Option World := do
  let before ← initial
  let activated ← activate before 1 6 ordinaryActivation 0 budget (some 11)
  let heldForBegin ← releaseAcquire activated
  Handover.beginProcessing heldForBegin 1 6 8

@[noinline] opaque retractedPhase : Option World := do
  let begun ← begunPhase
  let heldForRaw ← releaseAcquire begun
  let rawState ← CompletionRetry.CanonicalGate.commitGate heldForRaw 1 6
    ⟨.append 8 providerRaw, rfl⟩
  let streamingState ← CompletionRetry.CanonicalGate.stepPolicy rawState 6 ⟨.issue, rfl⟩
  let failedState ← CompletionRetry.CanonicalGate.stepPolicy streamingState 6
    ⟨.observeFailure .transport "io" 7, rfl⟩
  let heldForRetract ← releaseAcquire failedState
  (commitProvider heldForRetract 1 6 (.retract 8 retryClose)).toOption

@[noinline] opaque delayedPhase : Option World := do
  let retracted ← retractedPhase
  let backingOff ← CompletionRetry.CanonicalGate.stepPolicy retracted 6
    ⟨.schedule, rfl⟩
  let delayed ← CompletionRetry.CanonicalGate.stepPolicy backingOff 9 ⟨.wake 9, rfl⟩
  pure delayed

@[noinline] opaque acceptedPhase : Option World := do
  let issuingState ← delayedPhase
  let streamingAgainState ← CompletionRetry.CanonicalGate.stepPolicy issuingState 9
    ⟨.issue, rfl⟩
  let heldForAccept ← releaseAcquire streamingAgainState
  (commitProvider heldForAccept 1 9 (.accept 8 acceptedClose acceptedMessage [] [])).toOption

@[noinline] opaque finishedPhase : Option World := do
  let accepted ← acceptedPhase
  let heldForTerminal ← releaseAcquire accepted
  let terminal ← CompletionRetry.CanonicalGate.commitGate heldForTerminal 1 9
    ⟨.terminalize 8 .completed (.message 813), rfl⟩
  let heldForFinish ← releaseAcquire terminal
  let (finished, acknowledged) ← finish heldForFinish 1
  if acknowledged != [] then none else pure finished

@[noinline] opaque activateRetryFinish : Option Bool := do
  let begun ← begunPhase
  let retracted ← retractedPhase
  let delayed ← delayedPhase
  let accepted ← acceptedPhase
  let finished ← finishedPhase
  pure (begun.requestId == 200 &&
    begun.queue.active == some 20 && begun.retry.phase == .issuing &&
    retracted.retry.phase == .retracted .transport "io" 7 &&
    delayed.retry.phase == .issuing && delayed.retry.attempt == 1 &&
    accepted.retry.phase == .accepted 813 &&
    finished.lease.request == .completed &&
    finished.terminalSelection == some (.message 813) &&
    finished.queue.active.isNone && finished.claimed.isNone &&
    finished.transcript.nextSeq == 1)

theorem actual_activation_retry_terminal_finish_composes :
    activateRetryFinish = some true := by native_decide

def activationTraceWitness : Option (World × World) := do
  let before ← initial
  let after ← activate before 1 6 ordinaryActivation 0 budget (some 11)
  pure (before, after)

theorem activationTraceWitness_isSome : activationTraceWitness.isSome = true := by
  native_decide

private theorem initial_seed_invariants (before : World)
    (h : initial = some before) :
    ClaimCoherent before ∧ SequenceBound before ∧ ClosureUnique before ∧
      toolProjectionCoherent before = true := by
  unfold initial at h
  cases hg : Gate.acquire (Gate.initial parentWorld) 1 true with
  | none => simp [hg] at h
  | some gate =>
      have hgate := Gate.acquire_preserves_durable_world
        (Gate.initial parentWorld) gate 1 true hg
      simp [hg] at h
      cases h
      refine ⟨idleClaimCoherent _ rfl rfl, ?_, ?_, ?_⟩
      · apply empty_messages_sequenceBound
        rw [hgate]
        rfl
      · intro coordinate left hleft
        rw [hgate] at hleft
        simp [Gate.initial, closures, sourceRecords, parentWorld] at hleft
      · apply empty_toolProjectionCoherent
        · rw [hgate]
          rfl
        · rw [hgate]
          rfl

/-- The executable fixture supplies an actual application `Trace.activate`,
not merely a second computation with the same endpoint. All four seed
predicates therefore reach the successfully activated world through their
trace theorems. -/
theorem actual_activation_trace_preserves_all_invariants :
    ∃ before after,
      Trace before after ∧
        ClaimCoherent before ∧ SequenceBound before ∧ ClosureUnique before ∧
          toolProjectionCoherent before = true ∧
        ClaimCoherent after ∧ SequenceBound after ∧ ClosureUnique after ∧
          toolProjectionCoherent after = true := by
  obtain ⟨pair, hwitness⟩ := Option.isSome_iff_exists.mp activationTraceWitness_isSome
  rcases pair with ⟨before, after⟩
  unfold activationTraceWitness at hwitness
  cases hi : initial with
  | none => simp [hi] at hwitness
  | some seeded =>
      simp [hi] at hwitness
      cases ha : activate seeded 1 6 ordinaryActivation 0 budget (some 11) with
      | none => simp [ha] at hwitness
      | some activated =>
          simp [ha] at hwitness
          rcases hwitness with ⟨rfl, rfl⟩
          have trace : Trace seeded activated := .activate 1 6 ordinaryActivation 0
            budget (some 11) ha
          have seed := initial_seed_invariants seeded hi
          exact ⟨seeded, activated, trace, seed.1, seed.2.1, seed.2.2.1, seed.2.2.2,
            trace.claimCoherent seed.1, trace.sequenceBound seed.2.1,
            trace.closureUnique seed.2.2.1, trace.toolProjectionCoherent seed.2.2.2⟩

namespace RunningRevocation

open CanonicalOutput.Execution.Examples

def seed : World :=
  let running :=
    ((acceptAndPublish (world 5) 7 providerTurn providerMessage [] [foregroundAdmission] >>=
      fun accepted => dispatch accepted 7 permit).toOption).getD (world 5)
  { running with retry := { running.retry with request := running.requestId } }

def held : World :=
  (Gate.acquire (Gate.initial seed) 1 true).getD (Gate.initial seed)

def after : Option World :=
  CompletionRetry.CanonicalGate.commitGate held 1 5
    ⟨.revoke 7 8 .dead (.message 501), rfl⟩

def checks : Option Bool := after.map fun post =>
  physicalRunning held 600 && decide (600 ∉ post.transcript.inFlight)

theorem after_isSome : after.isSome = true := by native_decide

theorem checks_hold : checks = some true := by native_decide

private theorem held_closureUnique : ClosureUnique held := by
  have hs : held.segments = [providerTurn] := by native_decide
  intro coordinate left hleft right hright
  simp [hs, closures, sourceRecords] at hleft hright
  exact hleft.1.trans hright.1.symm

/-- A real held-gate revocation of a physically running tool supplies an
actual `Trace.gate`. Both new invariants are transported by the trace theorems,
while the executable witness confirms the running pre-state and released
in-flight post-state. -/
theorem running_revocation_trace_preserves_new_invariants :
    ∃ post, Trace held post ∧ ClosureUnique held ∧
      toolProjectionCoherent held = true ∧ ClosureUnique post ∧
      toolProjectionCoherent post = true ∧ physicalRunning held 600 = true ∧
      600 ∉ post.transcript.inFlight := by
  obtain ⟨post, hpost⟩ := Option.isSome_iff_exists.mp after_isSome
  have hcommit : CompletionRetry.CanonicalGate.commitGate held 1 5
      ⟨.revoke 7 8 .dead (.message 501), rfl⟩ = some post := by
    simpa [after] using hpost
  have trace : Trace held post := .gate held post 1 5 _ hcommit
  have coherent : toolProjectionCoherent held = true := by native_decide
  have hchecks := checks_hold
  simp [checks, hpost] at hchecks
  exact ⟨post, trace, held_closureUnique, coherent,
    trace.closureUnique held_closureUnique, trace.toolProjectionCoherent coherent,
    hchecks.1, hchecks.2⟩

end RunningRevocation

/-- The application Goal entrypoint consumes a proof of the actual Goal owner
publication; the receipt cannot be supplied as an unjoined caller record. -/
@[noinline] opaque actualGoalActivation : Option Bool := do
  let state ← heldState
  match hp : GoalContinuation.publishGoalChild? claimedGoal state 1 5 claimedRequest
      (physicalBinding .active) goalEntry with
  | none => none
  | some result =>
      let held ← GoalContinuation.Cases.reacquire result.after
      let before : World :=
        { held with retry := initialRetry 999 5 0 budget (some 10) }
      let published : GoalPublication result :=
        ⟨claimedGoal, state, 1, 5, claimedRequest, physicalBinding .active, goalEntry, hp⟩
      let after ← activateGoal before 1 5 result published [] true 8 5 10 0 budget (some 10)
      pure (after.requestId == 200 &&
        after.queue.active == some 20 &&
        after.retry.request == 200 && after.retry.phase == .issuing)

theorem actual_goal_publication_enters_typed_application_activation :
    actualGoalActivation = some true := by native_decide

end CanonicalOutput.Execution.SessionComposition.Cases
