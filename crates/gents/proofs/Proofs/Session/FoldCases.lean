import Proofs.Session.Fold
import Proofs.PromptAssembly.CurrentInput

namespace SessionQueue.FoldCases

inductive Event where
  | enqueue (entry : QueueEntry)
  /-- Claim the queue head; `admitted` are the pending requests whose signed
  admission the claim verified. -/
  | claim (admitted : List RequestId)
  /-- The active turn publishes its next selected message. -/
  | consume
  | finish
  deriving Repr

def queue : SessionQueueState :=
  { scope := ⟨1, 900, some 1⟩, active := none, pending := [], terminal := ∅ }

/-- A user message. `queuedAfter` is set when it was admitted behind a busy
turn; `requester` 2 is a second signed principal in the same session. -/
def user (id : RequestId) (queuedAfter : Option RequestId := none)
    (requester : Option Nat := some 1) (turnContext : Nat := 0) : QueueEntry :=
  { requestId := id, createdAt := id, source := .user, policy := .append
  , queueKey := none, queuedAfter, requester, turnContext }

def steering (id : RequestId) (queuedAfter : RequestId) : QueueEntry :=
  { user id (some queuedAfter) with source := .steering }

structure Observation where
  active : Option RequestId
  pending : List RequestId
  /-- Selected by the active claim and not yet published. -/
  folding : List RequestId
  /-- Finished turns and published (superseded) messages. -/
  terminal : List RequestId
  /-- Per claim, in order: the claimed request and the run it selected. -/
  claims : List (RequestId × List RequestId)
  deriving DecidableEq, Repr

private structure RunState where
  queue : SessionQueueState
  claims : List (RequestId × List RequestId)

def step (state : RunState) : Event → Option RunState
  | .enqueue entry => do
      let next ← scopedStep? queue.scope state.queue (.appendPending entry)
      pure { state with queue := next }
  | .claim admitted =>
      match state.queue.pending with
      | [] => none
      | head :: rest => do
          let next ← scopedStep? queue.scope state.queue (.claimFolding admitted)
          let folded := (foldRun head admitted rest).map QueueEntry.requestId
          pure ⟨next, state.claims ++ [(head.requestId, folded)]⟩
  | .consume => do
      let next ← scopedStep? queue.scope state.queue .consumeFolded
      pure { state with queue := next }
  | .finish => do
      let next ← scopedStep? queue.scope state.queue .finishActive
      pure { state with queue := next }

def observation (events : List Event) : Option Observation := do
  let state ← events.foldlM step (⟨queue, []⟩ : RunState)
  let ids := (events.filterMap fun event => match event with
    | .enqueue entry => some entry.requestId
    | _ => none).mergeSort (fun a b => a ≤ b)
  pure ⟨state.queue.active, state.queue.pending.map QueueEntry.requestId,
    state.queue.folding.map QueueEntry.requestId,
    ids.filter (fun id => decide (id ∈ state.queue.terminal)), state.claims⟩

def cases : List (String × List Event) :=
  [ ("queued_user_messages_fold_into_claim",
      [.enqueue (user 101), .enqueue (user 102 (some 101)), .enqueue (user 103 (some 101)),
        .claim [102, 103], .consume, .consume])
  , ("message_after_claim_waits_for_next_turn",
      [.enqueue (user 101), .enqueue (user 102 (some 101)), .claim [102], .consume,
        .enqueue (user 103 (some 101)), .enqueue (user 104 (some 101)), .finish,
        .claim [104]])
  , ("early_finish_returns_selection_to_queue",
      [.enqueue (user 101), .enqueue (user 102 (some 101)), .enqueue (user 103 (some 101)),
        .claim [102, 103], .finish, .claim [103]])
  , ("finish_after_partial_publication_returns_the_rest",
      [.enqueue (user 101), .enqueue (user 102 (some 101)), .enqueue (user 103 (some 101)),
        .claim [102, 103], .consume, .finish])
  , ("foreign_requester_stops_fold",
      [.enqueue (user 101), .enqueue (user 102 (some 101) (some 2)),
        .enqueue (user 103 (some 101)), .claim [102, 103]])
  , ("agent_steering_stops_fold",
      [.enqueue (user 101), .enqueue (steering 102 101), .enqueue (user 103 (some 101)),
        .claim [102, 103]])
  , ("changed_settings_stop_fold",
      [.enqueue (user 101), .enqueue (user 102 (some 101) (turnContext := 1)),
        .claim [102]])
  , ("unverified_admission_stops_fold",
      [.enqueue (user 101), .enqueue (user 102 (some 101)), .enqueue (user 103 (some 101)),
        .claim [103]])
  , ("steering_head_folds_nothing",
      [.enqueue (steering 101 100), .enqueue (user 102 (some 100)), .claim [102]]) ]

example : cases.map (fun c => observation c.2) =
    [ some ⟨some 101, [], [], [102, 103], [(101, [102, 103])]⟩
    , some ⟨some 103, [], [104], [101, 102], [(101, [102]), (103, [104])]⟩
    , some ⟨some 102, [], [103], [101], [(101, [102, 103]), (102, [103])]⟩
    , some ⟨none, [103], [], [101, 102], [(101, [102, 103])]⟩
    , some ⟨some 101, [102, 103], [], [], [(101, [])]⟩
    , some ⟨some 101, [102, 103], [], [], [(101, [])]⟩
    , some ⟨some 101, [102], [], [], [(101, [])]⟩
    , some ⟨some 101, [102, 103], [], [], [(101, [])]⟩
    , some ⟨some 101, [102], [], [], [(101, [])]⟩ ] := by native_decide

/-- Executable folded-turn inputs; native tests take their authored entries and
provider order from `TurnInput.authored` and `TurnInput.providerInput`. -/
def turnInputCases : List (String × TurnInput String) :=
  [ ("head_then_folded_in_queue_order",
      ⟨none, "how are we looking", [(102, "we should move faster"), (103, "and ship")]⟩)
  , ("context_precedes_head_and_folded", ⟨some "request context", "first", [(102, "second")]⟩)
  , ("unfolded_turn", ⟨none, "only message", []⟩) ]

example : turnInputCases.all (fun (_, input) =>
    input.providerInput == input.authored.map Prod.snd) = true := by native_decide

/-- A user retry whose claim selected queued messages. Whether it resumes is
`CurrentInput.admitResume` for a same-session, same-requester terminal
parent with settled tools; which selected messages it answers is
`CurrentInput.answersSelection`. -/
structure RetrySelectionCase where
  name : String
  parentPublished : Bool
  selected : List RequestId
  deriving Repr

def RetrySelectionCase.resume (value : RetrySelectionCase) : Bool :=
  (PromptAssembly.CurrentInput.admitResume true true true true value.parentPublished).getD false

def RetrySelectionCase.answered (value : RetrySelectionCase) : List RequestId :=
  PromptAssembly.CurrentInput.answersSelection value.resume value.selected

def retrySelectionCases : List RetrySelectionCase :=
  [ ⟨"resumed_retry_leaves_its_selection_queued", true, [104]⟩
  , ⟨"fresh_retry_answers_its_selection", false, [104]⟩ ]

example : retrySelectionCases.map (fun c => (c.resume, c.answered)) =
    [(true, []), (false, [104])] := by native_decide

end SessionQueue.FoldCases
