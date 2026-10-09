import Proofs.CanonicalOutput.Execution.HandoverCases

/-! Composed folded-turn scripts through the handover and gate owners: claim
with a verified selection, authored publication of the prompt and each
selected message, lease recovery under a fresh generation, and finish. Each
step records whether the owner accepted it, the session queue, and the
request's authored transcript keys. Native tests replay the same scripts
through the native claim, publication, recovery and terminal owners. -/
namespace CanonicalOutput.Execution.FoldPublication

open Handover Handover.Cases CanonicalOutput.Execution.Examples

/-- The selected message the head's turn answers. -/
def selectedEntry : SessionQueue.QueueEntry := followingEntry 903 7

inductive Step where
  /-- Publish the head's own prompt under `generation`. -/
  | publishPrompt (generation : Generation)
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
  , active := world.queue.active
  , pending := world.queue.pending.map (·.requestId)
  , folding := world.queue.folding.map (·.requestId)
  , terminal := [902, 903].filter (fun id => decide (id ∈ world.queue.terminal))
  , authoredKeys := (world.messages.filter fun message =>
      message.header.request == some world.requestId && message.header.role == .user).map
        (·.key) }

/-- Claim the head with `[903]` verified, then begin its execution. -/
def claimed : Option World := do
  let parent ← terminalRunningParent
  let gate ← Gate.acquire (Gate.initial parent) 1 true
  let queue : SessionQueue.SessionQueueState :=
    { scope := ⟨1, 1, none⟩, active := none, pending := [nextEntry, selectedEntry]
    , terminal := ∅ }
  let world ← claimAndActivate { gate with queue := queue, claimed := none } 1 6
    { nextActivation with admitted := [903] }
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

end CanonicalOutput.Execution.FoldPublication
