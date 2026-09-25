import Proofs.Session.Interrupt

namespace SessionQueue.InterruptCases

inductive Event where
  | interrupt
  | captureInterrupt
  | commitInterrupt
  | enqueue (entry : QueueEntry)
  deriving Repr

def request : RequestContext :=
  { state := .processing, origin := .interactive, backend := ⟨"interrupt-test"⟩
  , admission := .executing, deadline := 100, claimTime := 0, currentTime := 10
  , retryCount := 0, maxRetries := 3, messageSeq := 0, persistence := .uncommitted }

def queue : SessionQueueState :=
  { scope := ⟨1, 900, some 1⟩, active := some 100, pending := [], terminal := ∅ }

def wake (id : RequestId) (key : QueueKey := 900) : QueueEntry :=
  { requestId := id, createdAt := 10, source := .backgroundCompletion
  , policy := .coalesce, queueKey := some key, queuedAfter := none
  , origin := .scheduled }

def user : QueueEntry :=
  { requestId := 302, createdAt := 10, source := .user
  , policy := .append, queueKey := none, queuedAfter := none }

def interactiveMetadata : QueueEntry :=
  { wake 304 with origin := .interactive }

private structure RunState where
  request : RequestContext
  queue : SessionQueueState
  captured : Option (List RequestId)

def step (state : RunState) : Event → Option RunState
  | .interrupt => do
      let (request, next) ← latchInterruptScoped queue.scope state.request state.queue
      pure ⟨request, next, none⟩
  | .captureInterrupt =>
      if state.request.interruptRequestedAt.isSome then some state
      else some { state with captured := some (state.queue.pending.map QueueEntry.requestId) }
  | .commitInterrupt => do
      let observed ← state.captured
      let (request, next) ← latchInterruptObservedScoped queue.scope
        state.request observed state.queue
      pure ⟨request, next, none⟩
  | .enqueue entry => do
      let next ← scopedStep? queue.scope state.queue
        (if entry.policy == .coalesce then .coalescePending entry else .appendPending entry)
      pure { state with queue := next }

def run (events : List Event) : Option (RequestContext × SessionQueueState) :=
  (events.foldlM step (⟨request, queue, none⟩ : RunState)).map
    fun state => (state.request, state.queue)

def cases : List (String × List Event) :=
  [ ("completion_before_interrupt", [.enqueue (wake 301), .interrupt])
  , ("completion_after_interrupt", [.interrupt, .enqueue (wake 301)])
  , ("interrupt_replay_preserves_later_same_key",
      [.enqueue (wake 301), .interrupt, .enqueue (wake 303), .interrupt])
  , ("coalesced_before_interrupt",
      [.enqueue (wake 301), .enqueue (wake 303), .interrupt])
  , ("user_and_other_wake_keys",
      [.enqueue (wake 301), .enqueue user, .enqueue (wake 303 901), .interrupt])
  , ("same_time_after_interrupt_is_not_old",
      [.enqueue (wake 301), .interrupt, .enqueue (wake 303), .enqueue user])
  , ("empty_scan_completion_before_latch_commit",
      [.captureInterrupt, .enqueue (wake 301), .commitInterrupt, .interrupt])
  , ("observed_old_then_later_same_key_completion",
      [.enqueue (wake 301), .captureInterrupt, .commitInterrupt,
        .enqueue (wake 303), .interrupt])
  , ("interactive_background_metadata_preserved",
      [.enqueue interactiveMetadata, .interrupt]) ]

def observation (events : List Event) : Option (List RequestId × List RequestId × Bool) := do
  let state ← run events
  let ids := (events.filterMap fun event => match event with
    | .enqueue entry => some entry.requestId
    | .interrupt | .captureInterrupt | .commitInterrupt => none).dedup.mergeSort (fun a b => a ≤ b)
  pure (state.2.pending.map QueueEntry.requestId,
    ids.filter (fun id => decide (id ∈ state.2.terminal)),
    state.1.interruptRequestedAt.isSome)

example : cases.map (fun c => observation c.2) =
    [ some ([], [301], true)
    , some ([301], [], true)
    , some ([303], [301], true)
    , some ([], [301], true)
    , some ([302], [301, 303], true)
    , some ([303, 302], [301], true)
    , some ([301], [], true)
    , some ([303], [301], true)
    , some ([304], [], true) ] := by native_decide

example : cases.all (fun c => (run c.2).isSome) = true := by native_decide

end SessionQueue.InterruptCases
