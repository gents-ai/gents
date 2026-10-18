import Proofs.StorageWriteGate

namespace StorageWriteGate.StreamWaiter

/-- A polled lock future may queue and its place reserved before it is polled
    again; dropping a borrowed `next()` leaves the generator alive while dropping
    the generator cancels its pending acquire. This finite model assumes queued
    acquisition is FIFO and cancellation returns its queue reservation only. -/
inductive Waiter where
  | idle | queued | reserved | acquired | cancelled
  deriving DecidableEq, Repr

inductive Owner where
  | stream | flush
  deriving DecidableEq, Repr

structure State where
  gateHeld : Bool
  initialGuardHeld : Bool
  generatorAlive : Bool
  borrowedNext : Bool
  consumerWaiting : Bool
  streamWaiter : Waiter
  flushWaiter : Waiter
  queue : List Owner
  bufferedItem : Bool
  streamAcquired : Bool
  deriving DecidableEq, Repr

inductive Event where
  | pollNext | consumerWait | dropBorrowedNext | releaseInitialGuard
  | flushPoll | progressPoll | progressFlush | streamWriteReturned | flushWriteReturned
  | dropGenerator | flushOnlyPoll
  deriving DecidableEq, Repr

def grant (s : State) : State :=
  if s.gateHeld || s.streamWaiter == .reserved || s.flushWaiter == .reserved then s
  else match s.queue with
    | .stream :: rest => { s with streamWaiter := .reserved, queue := rest }
    | .flush :: rest => { s with flushWaiter := .reserved, queue := rest }
    | [] => s

def enqueue (s : State) (owner : Owner) : State :=
  { s with queue := s.queue ++ [owner] }

def without (queue : List Owner) (owner : Owner) : List Owner :=
  queue.filter (· != owner)

def step (s : State) (event : Event) : State :=
  match event with
  | .pollNext =>
      if !s.generatorAlive || s.streamWaiter != .idle || s.bufferedItem then s
      else if s.gateHeld || !s.queue.isEmpty || s.streamWaiter == .reserved ||
          s.flushWaiter == .reserved then
        let waiting := enqueue { s with borrowedNext := true, streamWaiter := .queued } .stream
        grant waiting
      else { s with
        borrowedNext := true
        streamWaiter := .acquired
        gateHeld := true
        streamAcquired := true }
  | .consumerWait => { s with consumerWaiting := true }
  | .dropBorrowedNext => { s with borrowedNext := false }
  | .releaseInitialGuard =>
      if s.initialGuardHeld then grant { s with gateHeld := false, initialGuardHeld := false }
      else s
  | .flushPoll =>
      if !s.consumerWaiting || s.flushWaiter != .idle then s
      else if s.gateHeld || !s.queue.isEmpty || s.streamWaiter == .reserved ||
          s.flushWaiter == .reserved then
        let waiting := enqueue { s with flushWaiter := .queued } .flush
        grant waiting
      else { s with flushWaiter := .acquired, gateHeld := true }
  | .progressPoll =>
      if s.generatorAlive && s.consumerWaiting && !s.bufferedItem &&
          s.streamWaiter == .reserved then
        { s with
          streamWaiter := .acquired
          borrowedNext := true
          gateHeld := true
          streamAcquired := true }
      else s
  | .progressFlush =>
      if s.consumerWaiting && s.flushWaiter == .reserved then
        { s with flushWaiter := .acquired, gateHeld := true }
      else s
  | .streamWriteReturned =>
      if s.streamWaiter != .acquired then s
      else grant ({ s with
        streamWaiter := .idle
        borrowedNext := false
        gateHeld := false
        bufferedItem := true })
  | .flushWriteReturned =>
      if s.flushWaiter != .acquired then s
      else grant ({ s with
        flushWaiter := .idle
        gateHeld := false
        consumerWaiting := false })
  | .dropGenerator =>
      let released := ({ s with
        generatorAlive := false
        borrowedNext := false
        streamWaiter := if s.streamWaiter == .queued || s.streamWaiter == .reserved
          then .cancelled else s.streamWaiter
        queue := without s.queue .stream })
      grant released
  | .flushOnlyPoll =>
      if s.consumerWaiting && s.flushWaiter == .reserved then
        { s with flushWaiter := .acquired, gateHeld := true }
      else s

def run (s : State) (events : List Event) : State := events.foldl step s

def waiting : State := ⟨true, true, true, false, false, .idle, .idle, [], false, false⟩

def queuedBehindWriter : State :=
  run waiting [.pollNext, .consumerWait, .flushPoll]

def reservedAfterWriter : State := step queuedBehindWriter .releaseInitialGuard

theorem borrowed_next_drop_preserves_reserved_acquire :
    let after := step reservedAfterWriter .dropBorrowedNext
    after.streamWaiter = .reserved ∧ after.generatorAlive = true ∧
      after.flushWaiter = .queued ∧ after.queue = [.flush] := by
  decide

theorem flush_only_poll_cannot_progress_reserved_generator :
    step reservedAfterWriter .flushOnlyPoll = reservedAfterWriter := by decide

theorem dropping_generator_cancels_reservation_and_unblocks_flush :
    let after := step (step reservedAfterWriter .dropBorrowedNext) .dropGenerator
    after.streamWaiter = .cancelled ∧ after.flushWaiter = .reserved ∧
      after.queue = [] := by
  decide

theorem fifo_grants_flush_when_it_queued_first :
    let after := run waiting [.consumerWait, .flushPoll, .pollNext, .releaseInitialGuard]
    after.flushWaiter = .reserved ∧ after.streamWaiter = .queued ∧
      after.queue = [.stream] := by
  decide

theorem initial_guard_release_cannot_release_stream_owner :
    let after := run reservedAfterWriter [.progressPoll, .releaseInitialGuard]
    after.streamWaiter = .acquired ∧ after.gateHeld = true := by
  decide

/-- Co-polling services one reserved stream acquire, then the completed stream
    write hands the mutex reservation to the waiting flush; the model bounds
    the helper to one buffered item. -/
def copolled : State :=
  run reservedAfterWriter
    [.progressPoll, .streamWriteReturned, .progressFlush]

theorem copolling_stream_allows_flush_to_acquire :
    copolled.flushWaiter = .acquired ∧ copolled.gateHeld = true ∧
      copolled.queue = [] := by
  decide

theorem buffered_item_prevents_second_progress :
    step copolled .progressPoll = copolled := by
  decide

end StorageWriteGate.StreamWaiter
