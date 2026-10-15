import Proofs.CanonicalOutput.Execution.HandoverCases
import Proofs.CanonicalOutput.Execution.Transition
import Proofs.Session.Management

/-! Composed folded-turn scripts through the handover and gate owners: claim
with a verified selection, authored publication of the prompt and each
selected message, lease recovery under a fresh generation, and finish. Each
step records whether the owner accepted it, the session queue, and the
request's authored transcript keys. Native tests replay the same scripts
through the native claim, publication, recovery and terminal owners. -/
namespace CanonicalOutput.Execution.FoldPublication

open Handover Handover.Cases CanonicalOutput.Execution.Examples

/-- The selected message the head's turn answers. -/
def activeEntry : SessionQueue.QueueEntry := { nextEntry with requester := some 1 }

def selectedEntry : SessionQueue.QueueEntry :=
  { followingEntry 903 7 with requester := some 1 }

/-- Intake is a queue selection under the same live execution fence as
publication. It neither publishes nor consumes input. The native safe-boundary
observation excludes an open provider stream; in-flight tools also block it. -/
private def selectSteering (world : World) (actor : Gate.Actor) (now : Time)
    (generation : Generation) (active : SessionQueue.QueueEntry)
    (admitted : List RequestId) (safeBoundary : Bool) : Option World := do
  if world.gateOwner != some actor || world.gateSchedule.phase != .storage ||
      !StorageWriteGate.pollable world.gateSchedule || now < world.lease.now ||
      !(CanonicalOutput.Execution.inputPublicationBeforeDeadline now world.retry.deadline) ||
      decide (world.transcript.inFlight ≠ ∅) then none else do
    let timed := Gate.atTime world now
    let _ ← RequestExecutionLease.step? timed.lease
      (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish)
    let queue ← SessionQueue.intakeSteering? world.queue.scope world.queue active
      admitted safeBoundary
    pure { timed with queue }

def intakeSteering (world : World) (actor : Gate.Actor) (now : Time)
    (generation : Generation) (active : SessionQueue.QueueEntry)
    (admitted : List RequestId) (safeBoundary : Bool) : Option World := do
  let selected ← selectSteering world actor now generation active admitted safeBoundary
  pure { selected with gateSchedule := { selected.gateSchedule with phase := .releasable } }

/-- Natural completion and late-input selection share one observed world and
execution fence. A selected prefix continues the same request; only an empty
prefix permits successful terminalization. Error and cancellation paths keep
their existing terminal owner. -/
def finishOrIntake (world : World) (actor : Gate.Actor) (now : Time)
    (generation : Generation) (active : SessionQueue.QueueEntry)
    (admitted : List RequestId) (safeBoundary : Bool)
    (selection : TerminalSelection) : Option World := do
  let selected ← selectSteering world actor now generation active admitted safeBoundary
  if selected.queue.folding.isEmpty then
    Gate.commit selected actor now (.terminalize generation .completed selection)
  else pure { selected with gateSchedule := { selected.gateSchedule with phase := .releasable } }

inductive Step where
  | observeDeadline (now deadline : Time)
  /-- Publish the head's own prompt under `generation`. -/
  | publishPrompt (generation : Generation)
  | enqueueSteering (requestId : RequestId)
  | cancelFirstPending
  | intake (generation : Generation) (safeBoundary : Bool)
  | finishOrIntake (generation : Generation) (safeBoundary : Bool)
  /-- Publish the prompt under `generation` with different content. -/
  | publishChangedPrompt (generation : Generation)
  /-- Publish selected message `requestId` under `generation`. -/
  | publishFolded (generation : Generation) (requestId : RequestId)
  /-- Accept a provider turn under `generation`. -/
  | acceptTurn (generation : Generation)
  /-- The lease expires; the request is recovered under `fresh`. -/
  | recover (expected fresh : Generation)
  | terminalize (generation : Generation)
  | finish
  deriving DecidableEq, Repr

structure Observation where
  accepted : Bool
  active : Option RequestId
  pending : List RequestId
  folding : List RequestId
  terminal : List RequestId
  authoredKeys : List String
  deriving DecidableEq, Repr

private structure RunState where
  world : World
  now : Time
  /-- The first accepted provider turn, replayed by later `acceptTurn` steps. -/
  turn : Option (Segment × MessageEnvelope)

private def held (world : World) : Option World := reacquire world

private def authoredSegment (world : World) (key : Nat) (generation : Generation)
    (now : Time) (byte : UInt8) : Segment :=
  { id := 1000 + 10 * key + world.segments.length
  , coordinate := ⟨world.requestId, .authored key⟩, writer := .request generation
  , flush := some ⟨0, [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩], [byte]⟩
  , close := some (.closed .complete 1 [1]), createdAt := now }

private def authoredMessage (world : World) (closing : Segment) (key : String)
    (generation : Generation) (now : Time) : MessageEnvelope :=
  { header :=
      { id := closing.id + 1, session := world.sessionId, request := some world.requestId
        origin := none, refs := [⟨closing.id, 0⟩], outcome := .complete, role := .user
        publication := .requestExecution generation }
    key, sequence := world.transcript.nextSeq, nativeId := none
    blocks := [.text ⟨⟨closing.id, 0⟩, .full⟩], createdAt := now }

/-- The entry already accepted under `key`, if any: replay reuses it. -/
private def accepted? (world : World) (key : String) : Option (Segment × MessageEnvelope) := do
  let message ← world.messages.find? (·.key == key)
  let ref ← message.header.refs.head?
  let closing ← world.segments.find? (·.id == ref.closeId)
  pure (closing, message)

private def authoredOperation (world : World) (key : String) (sourceKey : Nat)
    (generation : Generation) (now : Time) (byte : UInt8) : Gate.Operation :=
  match accepted? world key with
  | some (closing, message) =>
      if message.blocks == [.text ⟨⟨closing.id, 0⟩, .full⟩] &&
          closing.flush.map (·.payload) == some [byte] then
        .authored generation closing message
      else
        let closing := authoredSegment world sourceKey generation now byte
        .authored generation closing (authoredMessage world closing key generation now)
  | none =>
      let closing := authoredSegment world sourceKey generation now byte
      .authored generation closing (authoredMessage world closing key generation now)

private def commitStep (state : RunState) (operation : Gate.Operation) : Option RunState := do
  let ready ← held state.world
  let world ← Gate.commit ready 1 state.now operation
  pure { state with world }

private def turnSegment (world : World) (generation : Generation) (now : Time) : Segment :=
  { id := 2000 + world.segments.length, coordinate := ⟨world.requestId, .provider 0 0 0⟩
  , writer := .request generation, flush := none, close := some (.closed .complete 0 [])
  , createdAt := now }

private def turnMessage (world : World) (closing : Segment) (generation : Generation)
    (now : Time) : MessageEnvelope :=
  { header :=
      { id := closing.id + 1, session := world.sessionId, request := some world.requestId
        origin := none, refs := [], outcome := .complete, role := .assistant
        publication := .requestExecution generation }
    key := "assistant-0", sequence := world.transcript.nextSeq, nativeId := none, blocks := []
    createdAt := now }

/-- Apply one step. A rejected step leaves the durable world unchanged. -/
private def apply (state : RunState) : Step → RunState × Bool
  | .observeDeadline now deadline =>
      ({ state with now, world := { state.world with
          retry := { state.world.retry with deadline := some deadline } } }, true)
  | .enqueueSteering requestId =>
      let entry := { followingEntry requestId state.now with delivery := .steer, requester := some 1 }
      match SessionQueue.step? state.world.queue (.appendPending entry) with
      | some queue => ({ state with world := { state.world with queue } }, true)
      | none => (state, false)
  | .cancelFirstPending =>
      let queue := state.world.queue
      match SessionQueue.replacePendingGroup? queue.scope queue activeEntry.requester
          ((queue.folding ++ queue.pending).map (·.requestId)) 0 1 [] with
      | some queue => ({ state with world := { state.world with queue } }, true)
      | none => (state, false)
  | .intake generation safeBoundary =>
      match held state.world >>= fun ready =>
          intakeSteering ready 1 state.now generation activeEntry
            (ready.queue.pending.map (·.requestId)) safeBoundary with
      | some world => ({ state with world }, true)
      | none => (state, false)
  | .finishOrIntake generation safeBoundary =>
      match held state.world >>= fun ready =>
          finishOrIntake ready 1 state.now generation activeEntry
            (ready.queue.pending.map (·.requestId)) safeBoundary
            (match state.turn with
              | none => .noMessage
              | some (_, message) => .message message.header.id) with
      | some world => ({ state with world }, true)
      | none => (state, false)
  | .publishPrompt generation =>
      attempt (authoredOperation state.world "prompt" 0 generation state.now 65)
  | .publishChangedPrompt generation =>
      attempt (authoredOperation state.world "prompt" 0 generation state.now 66)
  | .publishFolded generation requestId =>
      attempt (authoredOperation state.world (foldedAuthoredKey requestId) requestId
        generation state.now 67)
  | .acceptTurn generation =>
      let (closing, message) := state.turn.getD
        (let closing := turnSegment state.world generation state.now
         (closing, turnMessage state.world closing generation state.now))
      match commitStep state (.accept generation closing message []) with
      | some next => ({ next with turn := some (closing, message) }, true)
      | none => (state, false)
  | .recover expected fresh =>
      let late := state.now + 20
      match commitStep { state with now := late }
          (.recover expected fresh 5 (late + 5) []) with
      | some next => (next, true)
      | none => (state, false)
  | .terminalize generation => attempt (.terminalize generation .completed .noMessage)
  | .finish =>
      match held state.world >>= (finishAndAcknowledge · 1) with
      | some result => ({ state with world := result.state }, true)
      | none => (state, false)
where
  attempt (operation : Gate.Operation) : RunState × Bool :=
    match commitStep state operation with
    | some next => (next, true)
    | none => (state, false)

private def observe (world : World) (accepted : Bool) : Observation :=
  { accepted
  , active := if isTerminal world.lease.request then none else world.queue.active
  , pending := world.queue.pending.map (·.requestId)
  , folding := world.queue.folding.map (·.requestId)
  , terminal := [902, 903].filter (fun id =>
      decide (id ∈ world.queue.terminal ∨ (world.claimed.map (·.logicalRequest) = some id ∧ isTerminal world.lease.request)))
  , authoredKeys := (world.messages.filter fun message =>
      message.header.request == some world.requestId && message.header.role == .user).map
        (·.key) }

/-- Claim the head with `[903]` verified, then begin its execution. -/
def claimed : Option World := do
  let parent ← terminalRunningParent
  let gate ← Gate.acquire (Gate.initial parent) 1 true
  let queue : SessionQueue.SessionQueueState :=
    { scope := ⟨1, 1, some 1⟩, active := none, pending := [activeEntry, selectedEntry]
    , terminal := ∅ }
  let world ← claimAndActivate { gate with queue := queue, claimed := none } 1 6
    { nextActivation (some 1) with
      request := { nextAdmission (some 1) with entry := activeEntry }, admitted := [903] }
  let ready ← reacquire world
  beginProcessing ready 1 6 8

def run (steps : List Step) : Option (List Observation) := do
  let start ← claimed
  let (_, observations) := steps.foldl (fun (state, observed) step =>
    let (next, accepted) := apply state step
    (next, observed ++ [observe next.world accepted])) (({ world := start, now := 6, turn := none } : RunState), [])
  pure observations

def cases : List (String × List Step) :=
  [ ("full_publication_consumes_the_selection",
      [.publishPrompt 8, .publishFolded 8 903, .terminalize 8, .finish])
  , ("partial_publication_returns_the_rest_on_finish",
      [.publishPrompt 8, .terminalize 8, .finish])
  , ("reclaim_reuses_accepted_input_under_the_live_lease",
      [.publishPrompt 8, .publishFolded 8 903, .recover 8 9, .publishPrompt 9,
        .publishFolded 9 903, .publishPrompt 8, .publishChangedPrompt 9, .terminalize 9,
        .finish])
  , ("reclaim_between_publications_consumes_the_rest_once",
      [.publishPrompt 8, .recover 8 9, .publishPrompt 9, .publishFolded 9 903,
        .terminalize 9, .finish])
  , ("provider_turn_replay_is_generation_bound",
      [.acceptTurn 8, .recover 8 9, .acceptTurn 9]) ]

def steeringCases : List (String × List Step) :=
  [ ("late_input_same_request_after_provider_turn",
      [.publishPrompt 8, .publishFolded 8 903, .acceptTurn 8, .enqueueSteering 904,
       .intake 8 true, .publishFolded 8 904])
  , ("streaming_boundary_does_not_take_input",
      [.publishPrompt 8, .publishFolded 8 903, .enqueueSteering 904, .intake 8 false])
  , ("unpublished_intake_returns_on_finish",
      [.publishPrompt 8, .publishFolded 8 903, .enqueueSteering 904, .intake 8 true,
       .terminalize 8, .finish])
  , ("natural_completion_intakes_late_input_before_terminal",
      [.publishPrompt 8, .publishFolded 8 903, .acceptTurn 8, .enqueueSteering 904,
       .finishOrIntake 8 true, .publishFolded 8 904, .finishOrIntake 8 true, .finish])
  , ("natural_completion_during_stream_refuses",
      [.publishPrompt 8, .publishFolded 8 903, .enqueueSteering 904,
       .finishOrIntake 8 false])
  , ("cancel_selected_rejects_abandoned_publication",
      [.publishPrompt 8, .cancelFirstPending, .publishFolded 8 903])
  , ("consumed_input_cannot_be_cancelled",
      [.publishPrompt 8, .publishFolded 8 903, .cancelFirstPending])
  , ("stale_generation_cannot_take_input",
      [.publishPrompt 8, .publishFolded 8 903, .enqueueSteering 904, .recover 8 9,
       .intake 8 true, .intake 9 true, .publishFolded 9 904, .publishFolded 9 904])
  , ("deadline_equality_refuses_new_input",
      [.publishPrompt 8, .publishFolded 8 903, .enqueueSteering 904,
       .observeDeadline 7 7, .intake 8 true, .finishOrIntake 8 true])
  , ("expired_deadline_refuses_new_input",
      [.publishPrompt 8, .publishFolded 8 903, .enqueueSteering 904,
       .observeDeadline 8 7, .intake 8 true, .finishOrIntake 8 true])
  , ("deadline_equality_refuses_selected_publication",
      [.publishPrompt 8, .observeDeadline 7 7, .publishFolded 8 903])
  , ("expired_deadline_refuses_selected_publication",
      [.publishPrompt 8, .observeDeadline 8 7, .publishFolded 8 903])
  , ("published_input_replays_after_deadline",
      [.publishPrompt 8, .publishFolded 8 903, .observeDeadline 8 7,
       .publishFolded 8 903, .publishPrompt 8])
  , ("provider_audit_publication_survives_request_deadline",
      [.publishPrompt 8, .publishFolded 8 903, .observeDeadline 8 7, .acceptTurn 8])
  ]

theorem request_deadline_blocks_only_new_input :
    ((steeringCases.drop 8).map fun c =>
      (((run c.2).getD []).getLast?).map (·.accepted)) =
      [some false, some false, some false, some false, some true, some true] := by
  native_decide

private def summary (observation : Observation) :
    Bool × List RequestId × List RequestId × List RequestId × List String :=
  (observation.accepted, observation.pending, observation.folding, observation.terminal,
    observation.authoredKeys)

/-- Consumption is part of the authored commit, finishing returns only what
was not published, and reuse is exact and fenced by the live lease. -/
theorem composed_scripts_consume_publish_and_reuse :
    ((cases.map fun c => ((run c.2).getD []).map summary).map (·.getLast?) ==
    [ some (true, [], [], [902, 903], ["prompt", "folded:903"])
    , some (true, [903], [], [902], ["prompt"])
    , some (true, [], [], [902, 903], ["prompt", "folded:903"])
    , some (true, [], [], [902, 903], ["prompt", "folded:903"])
    , some (false, [], [903], [], []) ]) = true := by native_decide

theorem stale_and_changed_reuse_are_rejected :
    (((run (cases[2]!).2).getD []).map (·.accepted) ==
      [true, true, true, true, true, false, false, true, true]) = true := by native_decide

theorem steering_keeps_physical_request_and_consumes_once :
    (((run (steeringCases[0]!).2).getD []).getLast?.map fun o =>
      (o.accepted, o.active, o.pending, o.folding, o.authoredKeys)) =
      some (true, some 902, [], [], ["prompt", "folded:903", "folded:904"]) := by
  native_decide

theorem natural_completion_selects_before_terminalizing :
    (((run (steeringCases[3]!).2).getD [])[4]?.map fun o =>
      (o.accepted, o.active, o.pending, o.folding, o.terminal)) =
      some (true, some 902, [], [904], [903]) := by native_decide

theorem natural_completion_terminalizes_before_queue_cleanup :
    (((run (steeringCases[3]!).2).getD [])[6]?.map fun o =>
      (o.accepted, o.active, o.pending, o.folding, o.terminal)) =
      some (true, none, [], [], [902, 903]) := by native_decide

theorem completion_after_publication_finishes_same_request :
    (((run (steeringCases[3]!).2).getD []).getLast?.map fun o =>
      (o.accepted, o.active, o.pending, o.folding, o.authoredKeys)) =
      some (true, none, [], [], ["prompt", "folded:903", "folded:904"]) := by native_decide

theorem cancelled_selection_cannot_publish :
    (((run (steeringCases[5]!).2).getD []).getLast?.map fun o =>
      (o.accepted, o.folding, o.authoredKeys)) =
      some (false, [], ["prompt"]) := by native_decide

theorem consumed_selection_cannot_be_edited :
    (((run (steeringCases[6]!).2).getD []).getLast?.map fun o =>
      (o.accepted, o.folding, o.authoredKeys)) =
      some (false, [], ["prompt", "folded:903"]) := by native_decide

end CanonicalOutput.Execution.FoldPublication
