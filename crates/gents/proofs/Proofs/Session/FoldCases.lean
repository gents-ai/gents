import Proofs.Session.Fold

namespace SessionQueue.FoldCases

inductive Event where
  | enqueue (entry : QueueEntry)
  /-- Claim the queue head; `admitted` are the pending requests whose signed
  admission the claim verified. -/
  | claim (admitted : List RequestId)
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
  /-- Per claim, in order: the claimed request and the requests it folded. -/
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
  | .finish => do
      let next ← scopedStep? queue.scope state.queue .finishActive
      pure { state with queue := next }

def observation (events : List Event) : Option Observation := do
  let state ← events.foldlM step (⟨queue, []⟩ : RunState)
  pure ⟨state.queue.active, state.queue.pending.map QueueEntry.requestId, state.claims⟩

def cases : List (String × List Event) :=
  [ ("queued_user_messages_fold_into_claim",
      [.enqueue (user 101), .enqueue (user 102 (some 101)), .enqueue (user 103 (some 101)),
        .claim [102, 103]])
  , ("message_after_claim_waits_for_next_turn",
      [.enqueue (user 101), .enqueue (user 102 (some 101)), .claim [102],
        .enqueue (user 103 (some 101)), .enqueue (user 104 (some 101)), .finish,
        .claim [104]])
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
    [ some ⟨some 101, [], [(101, [102, 103])]⟩
    , some ⟨some 103, [], [(101, [102]), (103, [104])]⟩
    , some ⟨some 101, [102, 103], [(101, [])]⟩
    , some ⟨some 101, [102, 103], [(101, [])]⟩
    , some ⟨some 101, [102], [(101, [])]⟩
    , some ⟨some 101, [102, 103], [(101, [])]⟩
    , some ⟨some 101, [102], [(101, [])]⟩ ] := by native_decide

end SessionQueue.FoldCases
