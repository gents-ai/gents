import Proofs.Background.Transition
import Proofs.CodexShim.Projection

/-!
# Codex Shim Steering

Adapter-level contract for stock Codex `turn/steer` behavior.

This module intentionally does not model Codex itself. It only captures the
boundary facts the shim must preserve when translating Codex active-turn
steering into DEFRA request/session/transcript state:

* accepted steering is same-turn work, not a fresh Codex turn;
* accepted steering appends a durable `queue.source = steering` request;
* accepted steering commits the user message to the Codex-visible active turn;
  the durable DEFRA transcript append occurs when that queued request runs;
* interrupt steering is locally terminal for Codex and forwards the DEFRA
  request interrupt asynchronously.
-/

namespace CodexShim

abbrev TurnId := Nat

/-- The active Codex turn tracked by the shim for a DEFRA-backed thread. The
thread id is DEFRA's session id at this adapter boundary. -/
structure ActiveTurn where
  threadId : SessionId
  turnId : TurnId
  requestId : RequestId
  queuedSteering : List RequestId := []
  deriving DecidableEq, Repr

namespace ActiveTurn

def enqueueSteering (active : ActiveTurn) (requestId : RequestId) : ActiveTurn :=
  { active with queuedSteering := active.queuedSteering ++ [requestId] }

def drainSteering (active : ActiveTurn) (requestId : RequestId) (rest : List RequestId) :
    ActiveTurn :=
  { active with requestId := requestId, queuedSteering := rest }

end ActiveTurn

/-- Shim-owned protocol state relevant to steering. Counters abstract over the
actual streamed JSON-RPC notifications; preserving `turnStartedCount` is the
proof-level statement that `turn/steer` did not emit `turn/started`. -/
structure ShimState where
  active : Option ActiveTurn
  turnStartedCount : Nat
  turnCompletedCount : Nat
  committedUserMessageCount : Nat
  lastTerminalStatus : Option TurnPhase
  deriving DecidableEq, Repr

/-- Product state touched by the steering adapter. Queue, transcript, and
request state remain owned by their existing proof modules. -/
structure RuntimeState where
  shim : ShimState
  request : RequestContext
  queue : SessionQueue.SessionQueueState
  transcript : Transcript.TranscriptState

/-- Successful `turn/steer` against the active Codex turn.

The witness makes the earlier shim bug unrepresentable: an accepted steer must
preserve the active turn and the `turn/started` count while appending steering
work and committing a user message. -/
structure AcceptSteer
    (pre post : RuntimeState)
    (active : ActiveTurn)
    (expectedTurnId : TurnId)
    (steeringRequestId : RequestId)
    (steeringMessage : String) : Prop where
  h_active : pre.shim.active = some active
  h_expected : expectedTurnId = active.turnId
  h_queue_session : pre.queue.sessionId = active.threadId
  h_transcript_session : pre.transcript.sessionId = active.threadId
  h_request_preserved : post.request = pre.request
  h_active_updated :
    post.shim.active = some (active.enqueueSteering steeringRequestId)
  h_no_turn_started :
    post.shim.turnStartedCount = pre.shim.turnStartedCount
  h_user_message_committed :
    post.shim.committedUserMessageCount =
      pre.shim.committedUserMessageCount + 1
  h_queue_append :
    ∃ entry : SessionQueue.QueueEntry,
      SessionQueue.Transition pre.queue post.queue ∧
      post.queue = pre.queue.appendPending entry ∧
      entry.requestId = steeringRequestId ∧
      entry.source = SessionQueue.QueueSource.steering ∧
      entry.policy = SessionQueue.QueuePolicy.append ∧
      entry.queueKey = none ∧
      entry.queuedAfter = some active.requestId
  h_steering_message_nonempty :
    steeringMessage ≠ ""
  h_transcript_preserved_until_request_runs :
    post.transcript = pre.transcript

/-- Active-turn interrupt as seen by the Codex shim. This is just the existing
request interrupt signal lifted into the adapter state. The Codex-facing turn
is terminal locally; the core request interrupt is forwarded asynchronously and
is not required before the shim acknowledges `turn/interrupt`. -/
structure InterruptActive
    (pre post : RuntimeState)
    (active : ActiveTurn) : Prop where
  h_active : pre.shim.active = some active
  h_local_interrupt :
    TurnTransition .inProgress .interrupted .interrupt
  h_active_cleared : post.shim.active = none
  h_no_turn_started :
    post.shim.turnStartedCount = pre.shim.turnStartedCount
  h_turn_completed :
    post.shim.turnCompletedCount = pre.shim.turnCompletedCount + 1
  h_terminal_status :
    post.shim.lastTerminalStatus = some .interrupted
  h_request_eq : post.request = pre.request
  h_queue_eq : post.queue = pre.queue
  h_transcript_eq : post.transcript = pre.transcript

/-- Drain a queued steering request into the same active Codex turn.

This is the adapter witness for the stock Codex UX: a DEFRA request may finish,
but the Codex turn must remain in progress while accepted steering work remains
queued behind it. The shim advances its current DEFRA request pointer and does
not emit `turn/completed`. -/
structure DrainSteering
    (pre post : RuntimeState)
    (active : ActiveTurn)
    (observation : ProjectionObservation)
    (nextRequestId : RequestId)
    (rest : List RequestId) : Prop where
  h_active : pre.shim.active = some active
  h_queue_head : active.queuedSteering = nextRequestId :: rest
  h_observed_request : observation.requestState = pre.request.state
  h_projected_completed : projectObservation observation = .completed
  h_active_advanced :
    post.shim.active =
      some (active.drainSteering nextRequestId rest)
  h_no_turn_started :
    post.shim.turnStartedCount = pre.shim.turnStartedCount
  h_no_turn_completed :
    post.shim.turnCompletedCount = pre.shim.turnCompletedCount
  h_terminal_status_eq :
    post.shim.lastTerminalStatus = pre.shim.lastTerminalStatus
  h_user_messages_eq :
    post.shim.committedUserMessageCount =
      pre.shim.committedUserMessageCount
  h_request_eq : post.request = pre.request
  h_queue_eq : post.queue = pre.queue
  h_transcript_eq : post.transcript = pre.transcript

/-- The small adapter transition relation for the shim steering surface. -/
inductive Transition : RuntimeState → RuntimeState → Prop where
  | accept_steer
      {pre post : RuntimeState}
      {active : ActiveTurn}
      {expectedTurnId : TurnId}
      {steeringRequestId : RequestId}
      {steeringMessage : String} :
      AcceptSteer pre post active expectedTurnId steeringRequestId steeringMessage →
      Transition pre post
  | interrupt_active {pre post : RuntimeState} {active : ActiveTurn} :
      InterruptActive pre post active →
      Transition pre post
  | drain_steering
      {pre post : RuntimeState}
      {active : ActiveTurn}
      {observation : ProjectionObservation}
      {nextRequestId : RequestId}
      {rest : List RequestId} :
      DrainSteering pre post active observation nextRequestId rest →
      Transition pre post

theorem accept_steer_preserves_active_turn
    {pre post : RuntimeState}
    {active : ActiveTurn}
    {expectedTurnId : TurnId}
    {steeringRequestId : RequestId}
    {steeringMessage : String}
    (h : AcceptSteer pre post active expectedTurnId steeringRequestId steeringMessage) :
    ∃ postActive : ActiveTurn,
      post.shim.active = some postActive ∧
      postActive.threadId = active.threadId ∧
      postActive.turnId = active.turnId := by
  refine ⟨active.enqueueSteering steeringRequestId, h.h_active_updated, ?_, ?_⟩ <;> rfl

theorem accept_steer_records_queued_request
    {pre post : RuntimeState}
    {active : ActiveTurn}
    {expectedTurnId : TurnId}
    {steeringRequestId : RequestId}
    {steeringMessage : String}
    (h : AcceptSteer pre post active expectedTurnId steeringRequestId steeringMessage) :
    ∃ postActive : ActiveTurn,
      post.shim.active = some postActive ∧
      postActive.queuedSteering = active.queuedSteering ++ [steeringRequestId] := by
  refine ⟨active.enqueueSteering steeringRequestId, h.h_active_updated, ?_⟩
  rfl

theorem accept_steer_does_not_emit_turn_started
    {pre post : RuntimeState}
    {active : ActiveTurn}
    {expectedTurnId : TurnId}
    {steeringRequestId : RequestId}
    {steeringMessage : String}
    (h : AcceptSteer pre post active expectedTurnId steeringRequestId steeringMessage) :
    post.shim.turnStartedCount = pre.shim.turnStartedCount :=
  h.h_no_turn_started

theorem accept_steer_appends_steering_entry
    {pre post : RuntimeState}
    {active : ActiveTurn}
    {expectedTurnId : TurnId}
    {steeringRequestId : RequestId}
    {steeringMessage : String}
    (h : AcceptSteer pre post active expectedTurnId steeringRequestId steeringMessage) :
    ∃ entry : SessionQueue.QueueEntry,
      entry.requestId = steeringRequestId ∧
      entry.source = SessionQueue.QueueSource.steering ∧
      entry.policy = SessionQueue.QueuePolicy.append ∧
      entry.queueKey = none ∧
      entry.queuedAfter = some active.requestId := by
  rcases h.h_queue_append with
    ⟨entry, _h_transition, _h_post, h_request, h_source, h_policy, h_key, h_after⟩
  exact ⟨entry, h_request, h_source, h_policy, h_key, h_after⟩

theorem interrupt_active_clears_active_turn
    {pre post : RuntimeState}
    {active : ActiveTurn}
    (h : InterruptActive pre post active) :
    post.shim.active = none :=
  h.h_active_cleared

theorem interrupt_active_emits_terminal_turn
    {pre post : RuntimeState}
    {active : ActiveTurn}
    (h : InterruptActive pre post active) :
    post.shim.turnCompletedCount = pre.shim.turnCompletedCount + 1 ∧
      post.shim.lastTerminalStatus = some .interrupted := by
  exact ⟨h.h_turn_completed, h.h_terminal_status⟩

theorem interrupt_active_does_not_wait_for_request_transition
    {pre post : RuntimeState}
    {active : ActiveTurn}
    (h : InterruptActive pre post active) :
    post.request = pre.request :=
  h.h_request_eq

theorem interrupt_active_does_not_preserve_active_turn
    {pre post : RuntimeState}
    {active : ActiveTurn}
    (h : InterruptActive pre post active) :
    post.shim.active ≠ pre.shim.active := by
  rw [h.h_active, h.h_active_cleared]
  simp

theorem drain_steering_advances_active_request_without_completing_turn
    {pre post : RuntimeState}
    {active : ActiveTurn}
    {observation : ProjectionObservation}
    {nextRequestId : RequestId}
    {rest : List RequestId}
    (h : DrainSteering pre post active observation nextRequestId rest) :
    ∃ postActive : ActiveTurn,
      post.shim.active = some postActive ∧
      postActive.requestId = nextRequestId ∧
      post.shim.turnCompletedCount = pre.shim.turnCompletedCount := by
  refine ⟨active.drainSteering nextRequestId rest, h.h_active_advanced, ?_, h.h_no_turn_completed⟩
  rfl

theorem drain_steering_uses_completed_projection
    {pre post : RuntimeState}
    {active : ActiveTurn}
    {observation : ProjectionObservation}
    {nextRequestId : RequestId}
    {rest : List RequestId}
    (h : DrainSteering pre post active observation nextRequestId rest) :
    projectObservation observation = .completed :=
  h.h_projected_completed

end CodexShim
