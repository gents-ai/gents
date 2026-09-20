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

def actualSessionIdle (state : Handover.State) : Bool :=
  state.claimed.isNone && state.paired.queue.active.isNone && state.paired.queue.pending.isEmpty

structure Result where
  beforeGoal : Snapshot
  afterGoal : Snapshot
  before : Handover.State
  after : Handover.State
  request : ClaimedRequest
  binding : Binding
  entry : SessionQueue.QueueEntry
  outcome : GoalAutomation.OperatorResume.Outcome
  ownerPublication : publishClaimed beforeGoal request true = (afterGoal, outcome)
  allowed : outcome = .created ∨ outcome = .recovered
  freshEnqueued : outcome = .created →
    SessionQueue.step? before.paired.queue (.coalescePending entry) =
      some after.paired.queue
  replayInert : outcome = .recovered → after = before
  nextSeqPreserved : after.paired.gate.execution.transcript.nextSeq =
    before.paired.gate.execution.transcript.nextSeq

def gateHeld (state : Handover.State) (actor : Gate.Actor) (now : Time) : Bool :=
  state.paired.gate.owner == some actor && state.paired.gate.schedule.phase == .storage &&
    StorageWriteGate.pollable state.paired.gate.schedule &&
    state.paired.gate.execution.lease.now ≤ now

def releaseGate (state : Handover.State) (now : Time) : Handover.State :=
  let gate := state.paired.gate
  { state with paired := { state.paired with gate :=
      { execution := Gate.atTime gate.execution now
      , owner := gate.owner
      , schedule := { gate.schedule with phase := .releasable } } } }

def publishGoalChild? (goal : Snapshot) (state : Handover.State)
    (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest) (binding : Binding)
    (entry : SessionQueue.QueueEntry) : Option Result :=
  let world := state.paired.gate.execution
  if !gateHeld state actor now || !bindingValid world goal request binding ||
      state.paired.queue.scope.agent != world.principal ||
      state.paired.queue.sessionId != world.sessionId ||
      binding.childRequester != state.paired.queue.scope.requester ||
      !goalEntryValid world request binding entry then none
  else
    match hp : publishClaimed goal request true with
    | (post, .created) =>
        if !canonicalParentTerminal world binding || !actualSessionIdle state ||
            !request.terminalParent || !request.sessionIdle then none
        else match hstep : SessionQueue.step? state.paired.queue (.coalescePending entry) with
        | none => none
        | some queue => some
            { beforeGoal := goal, afterGoal := post, before := state
            , after := { (releaseGate state now) with paired :=
                { (releaseGate state now).paired with queue := queue } }
            , request := request, binding := binding, entry := entry
            , outcome := .created, ownerPublication := hp
            , allowed := Or.inl rfl
            , freshEnqueued := fun _ => hstep
            , replayInert := by simp, nextSeqPreserved := rfl }
    | (post, .recovered) =>
        -- Lost acknowledgement after the atomic Goal/request publication.  The
        -- exact child may already be pending, active, or terminal; do not create
        -- another queue row or claim it here.
        if entry ∈ state.paired.queue.pending ||
            state.paired.queue.active == some entry.requestId ||
            decide (entry.requestId ∈ state.paired.queue.terminal) then
          some
            { beforeGoal := goal, afterGoal := post, before := state
            , after := state
            , request := request, binding := binding, entry := entry
            , outcome := .recovered, ownerPublication := hp
            , allowed := Or.inr rfl
            , freshEnqueued := by simp
            , replayInert := fun _ => rfl, nextSeqPreserved := rfl }
        else none
    | _ => none

def childAdmission (result : Result) : Handover.PhysicalRequestAdmission :=
  { document := result.binding.childDocument
  , entry := result.entry
  , agent := result.binding.executionAgent
  , session := result.binding.executionSession
  , requester := result.binding.childRequester
  , authenticated := result.binding.authenticated }

def childActivation (result : Result) (configuredRoutes : List (DocId × Nat))
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

theorem successful_publication_uses_existing_goal_owner
    (goal : Snapshot) (state : Handover.State) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest)
    (binding : Binding) (entry : SessionQueue.QueueEntry) (result : Result)
    (h : publishGoalChild? goal state actor now request binding entry = some result) :
    publishClaimed goal request true = (result.afterGoal, result.outcome) := by
  unfold publishGoalChild? at h
  dsimp only at h
  repeat' split at h <;> try contradiction
  all_goals rcases h with ⟨_, ⟨_, rfl⟩⟩; assumption

theorem successful_publish_preserves_nextSeq
    (goal : Snapshot) (state : Handover.State) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest) (binding : Binding) (entry : SessionQueue.QueueEntry)
    (result : Result)
    (h : publishGoalChild? goal state actor now request binding entry = some result) :
    result.after.paired.gate.execution.transcript.nextSeq =
      state.paired.gate.execution.transcript.nextSeq := by
  unfold publishGoalChild? at h
  dsimp only at h
  repeat' split at h <;> try contradiction
  all_goals rcases h with ⟨_, ⟨_, rfl⟩⟩; rfl

theorem fresh_publication_uses_session_queue_owner
    (goal : Snapshot) (state : Handover.State) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest)
    (binding : Binding) (entry : SessionQueue.QueueEntry) (result : Result)
    (_h : publishGoalChild? goal state actor now request binding entry = some result)
    (hfresh : result.outcome = .created) :
    SessionQueue.step? result.before.paired.queue (.coalescePending result.entry) =
      some result.after.paired.queue := result.freshEnqueued hfresh

theorem replay_does_not_duplicate_queue_state
    (goal : Snapshot) (state : Handover.State) (actor : Gate.Actor) (now : Time)
    (request : ClaimedRequest)
    (binding : Binding) (entry : SessionQueue.QueueEntry) (result : Result)
    (_h : publishGoalChild? goal state actor now request binding entry = some result)
    (hreplay : result.outcome = .recovered) :
    result.after = result.before := result.replayInert hreplay

theorem paused_or_terminal_goal_cannot_fresh_publish
    (result : Result)
    (hstatus : result.beforeGoal.goal.status = .paused ∨
      result.beforeGoal.goal.status = .complete) :
    result.outcome ≠ .created := by
  intro hcreated
  have hpublication : (publishClaimed result.beforeGoal result.request true).2 = .created := by
    rw [result.ownerPublication, hcreated]
  have current := created_requires_current_claim result.beforeGoal result.request true
    hpublication
  rcases hstatus with hs | hs
  · rw [hs] at current
    rcases current.2.2.2.1 with hactive | hbudget <;> contradiction
  · rw [hs] at current
    rcases current.2.2.2.1 with hactive | hbudget <;> contradiction

end CanonicalOutput.Execution.GoalContinuation
