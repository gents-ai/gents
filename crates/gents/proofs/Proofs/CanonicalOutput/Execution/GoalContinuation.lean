import Proofs.CanonicalOutput.Execution.Handover
import Proofs.GoalAutomation.ClaimedPublication

/-!
# Goal-owned canonical continuation publication

This adapter composes the existing Goal claimed-publication owner with the
existing session queue and request handover owners.  It adds no Goal phase or
request lifecycle.  The native binding is one authenticated observation across
the differently typed physical, logical, owner, and session identity domains;
the model never casts one domain into another.
-/

namespace CanonicalOutput.Execution.GoalContinuation

open GoalAutomation.OperatorResume

/-- Authenticated native receipt joining the canonical Goal observation, the
terminal physical predecessor, and the prepared physical child request. -/
structure Binding where
  goalDocument : DocId
  goalOwner : String
  goalSession : String
  observedStatus : Goals.Status
  executionAgent : Nat
  executionSession : SessionId
  parentDocument : DocId
  parentLogical : RequestId
  childDocument : DocId
  childRequester : Option Nat
  childEntry : SessionQueue.QueueEntry
  /-- Must be projected from the same authenticated ConfigApplyTxn as the Goal,
  predecessor and child binding. Native must not supply an empty observation
  when a relevant accepted wait or background row is unreadable. -/
  observation : PublicationObservation
  authenticated : Bool
  deriving DecidableEq, Repr

def bindingValid (world : World) (snapshot : Snapshot)
    (request : ClaimedRequest) (binding : Binding) : Bool :=
  binding.authenticated && binding.executionAgent == world.principal &&
    binding.executionSession == world.sessionId &&
    binding.goalDocument == request.binding.goal &&
    binding.goalOwner == request.binding.owner &&
    binding.goalSession == request.binding.session &&
    binding.observedStatus == snapshot.goal.status &&
    request.expectedStatus == binding.observedStatus &&
    binding.parentDocument == request.binding.predecessorDoc &&
    binding.parentLogical == request.binding.predecessor &&
    binding.childDocument != binding.parentDocument

/-- The predecessor is not inferred from a message. Its exact physical request
row is terminal and carries the canonical terminal selection committed by the
request owner. -/
def canonicalParentTerminal (world : World) (binding : Binding) : Bool :=
  world.requestId == binding.parentDocument &&
    match world.lease.lease, world.terminalSelection with
    | .terminal _ outcome, some selection =>
        world.lease.request == outcome.requestState && terminalSelectionValid world selection
    | _, _ => false

def goalEntryValid (world : World) (request : ClaimedRequest)
    (binding : Binding) (entry : SessionQueue.QueueEntry) : Bool :=
  entry == binding.childEntry &&
    entry.requestId == request.binding.child && entry.requestId != binding.parentLogical &&
    entry.source == .goal && entry.policy == .coalesce &&
    entry.queueKey == some world.sessionId &&
    entry.queuedAfter == some binding.parentLogical &&
    decide (entry.coalesceWellFormed world.sessionId)

def actualSessionIdle (state : World) : Bool :=
  state.claimed.isNone && state.queue.active.isNone && state.queue.pending.isEmpty

structure Result where
  beforeGoal : Snapshot
  afterGoal : Snapshot
  before : World
  after : World
  request : ClaimedRequest
  observation : PublicationObservation
  binding : Binding
  entry : SessionQueue.QueueEntry
  outcome : GoalAutomation.OperatorResume.Outcome
  ownerPublication : publishClaimed beforeGoal request observation true = (afterGoal, outcome)
  allowed : outcome = .created ∨ outcome = .recovered
  freshEnqueued : outcome = .created →
    SessionQueue.step? before.queue (.coalescePending entry) =
      some after.queue
  replayInert : outcome = .recovered → after = before
  nextSeqPreserved : after.transcript.nextSeq =
    before.transcript.nextSeq

def gateHeld (state : World) (actor : Gate.Actor) (now : Time) : Bool :=
  state.gateOwner == some actor && state.gateSchedule.phase == .storage &&
    StorageWriteGate.pollable state.gateSchedule &&
    state.lease.now ≤ now

def releaseGate (state : World) (now : Time) : World :=
  { Gate.atTime state now with
    gateSchedule := { state.gateSchedule with phase := .releasable } }

def publishGoalChild? (goal : Snapshot) (state : World)
    (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest) (binding : Binding)
    (entry : SessionQueue.QueueEntry) : Option Result :=
  let world := state
  if state.purpose != .normal || !gateHeld state actor now ||
      !bindingValid world goal request binding ||
      state.queue.scope.agent != world.principal ||
      state.queue.sessionId != world.sessionId ||
      binding.childRequester != state.queue.scope.requester ||
      !goalEntryValid world request binding entry then none
  else
    match hp : publishClaimed goal request binding.observation true with
    | (post, .created) =>
        if !canonicalParentTerminal world binding || !actualSessionIdle state ||
            !request.terminalParent || !request.sessionIdle then none
        else match hstep : SessionQueue.step? state.queue (.coalescePending entry) with
        | none => none
        | some queue => some
            { beforeGoal := goal, afterGoal := post, before := state
            , after := { (releaseGate state now) with queue := queue }
            , request := request, observation := binding.observation, binding := binding, entry := entry
            , outcome := .created, ownerPublication := hp
            , allowed := Or.inl rfl
            , freshEnqueued := fun _ => hstep
            , replayInert := by simp, nextSeqPreserved := rfl }
    | (post, .recovered) =>
        -- Lost acknowledgement after the atomic Goal/request publication.  The
        -- exact child may already be pending, active, or terminal; do not create
        -- another queue row or claim it here.
        if entry ∈ state.queue.pending ||
            state.queue.active == some entry.requestId ||
            decide (entry.requestId ∈ state.queue.terminal) then
          some
            { beforeGoal := goal, afterGoal := post, before := state
            , after := state
            , request := request, observation := binding.observation, binding := binding, entry := entry
            , outcome := .recovered, ownerPublication := hp
            , allowed := Or.inr rfl
            , freshEnqueued := by simp
            , replayInert := fun _ => rfl, nextSeqPreserved := rfl }
        else none
    | _ => none

theorem successful_publication_preserves_claim_control
    (goal : Snapshot) (before : World) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest) (binding : Binding) (entry : SessionQueue.QueueEntry)
    (result : Result)
    (h : publishGoalChild? goal before actor now request binding entry = some result) :
    result.after.requestId = before.requestId ∧
      result.after.sessionId = before.sessionId ∧
      result.after.claimed = before.claimed ∧ result.after.retry = before.retry ∧
      result.after.queue.active = before.queue.active := by
  unfold publishGoalChild? at h
  dsimp only at h
  repeat' first | contradiction |
    (solve | cases h; simp_all [releaseGate, SessionQueue.step?, actualSessionIdle]) |
    split at h
  cases h
  refine ⟨rfl, rfl, rfl, rfl, ?_⟩
  apply SessionQueue.coalescePending_preserves_active
  assumption

def childAdmission (result : Result) : Handover.PhysicalRequestAdmission :=
  { document := result.binding.childDocument
  , entry := result.entry
  , agent := result.binding.executionAgent
  , session := result.binding.executionSession
  , requester := result.binding.childRequester
  , authenticated := result.binding.authenticated }

def childActivation (result : Result) (configuredRoutes : List (DocId × Nat × Nat))
    (routesAuthenticated : Bool) (generation : Generation)
    (duration deadline : Time) : Handover.Activation :=
  let admission := childAdmission result
  { request := admission
  , evidence := .goalChild
      { goalDocument := result.binding.goalDocument, request := admission
      , parentPhysical := result.binding.parentDocument
      , parentLogical := result.binding.parentLogical
      , authenticated := result.binding.authenticated }
  , configuredRoutes := configuredRoutes, routesAuthenticated := routesAuthenticated
  , generation := generation, duration := duration, deadline := deadline }

private theorem successful_publication_origin
    (goal : Snapshot) (state : World) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest)
    (binding : Binding) (entry : SessionQueue.QueueEntry) (result : Result)
    (h : publishGoalChild? goal state actor now request binding entry = some result) :
    result.beforeGoal = goal ∧ result.before = state ∧ result.request = request := by
  unfold publishGoalChild? at h
  dsimp only at h
  repeat' split at h <;> try contradiction
  all_goals rcases h with ⟨_, ⟨_, rfl⟩⟩; exact ⟨rfl, rfl, rfl⟩

theorem successful_publication_uses_existing_goal_owner
    (goal : Snapshot) (state : World) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest)
    (binding : Binding) (entry : SessionQueue.QueueEntry) (result : Result)
    (h : publishGoalChild? goal state actor now request binding entry = some result) :
    publishClaimed goal request result.observation true = (result.afterGoal, result.outcome) := by
  obtain ⟨hg, _, hr⟩ := successful_publication_origin goal state actor now request binding entry result h
  simpa only [hg, hr] using result.ownerPublication

theorem successful_publish_preserves_nextSeq
    (goal : Snapshot) (state : World) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest) (binding : Binding) (entry : SessionQueue.QueueEntry)
    (result : Result)
    (h : publishGoalChild? goal state actor now request binding entry = some result) :
    result.after.transcript.nextSeq =
      state.transcript.nextSeq := by
  have origin := successful_publication_origin goal state actor now request binding entry result h
  simpa only [origin.2.1] using result.nextSeqPreserved

theorem paused_or_terminal_goal_cannot_fresh_publish
    (result : Result)
    (hstatus : result.beforeGoal.goal.status = .paused ∨
      result.beforeGoal.goal.status = .complete) :
    result.outcome ≠ .created := by
  intro hcreated
  have hpublication :
      (publishClaimed result.beforeGoal result.request result.observation true).2 = .created := by
    rw [result.ownerPublication, hcreated]
  have current := created_requires_current_claim result.beforeGoal result.request result.observation true
    hpublication
  rcases hstatus with hs | hs
  · rw [hs] at current
    rcases current.2.2.2.1 with hactive | hbudget <;> contradiction
  · rw [hs] at current
    rcases current.2.2.2.1 with hactive | hbudget <;> contradiction

end CanonicalOutput.Execution.GoalContinuation
