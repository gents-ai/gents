import Proofs.StorageWriteGate.StreamWaiter
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.StorageWriteGateStreamWaiter
open StorageWriteGate.StreamWaiter Conformance.Contracts

structure Case where
  name : String
  events : List Event

def borrowedDropCase : Case :=
  ⟨"borrowed_next_drop_stalls_reserved_stream",
    [.pollNext, .consumerWait, .flushPoll, .releaseInitialGuard,
      .dropBorrowedNext, .flushOnlyPoll]⟩

def generatorDropCase : Case :=
  ⟨"generator_drop_cancels_reservation",
    [.pollNext, .consumerWait, .flushPoll, .releaseInitialGuard,
      .dropBorrowedNext, .dropGenerator, .progressFlush]⟩

def copollCase : Case :=
  ⟨"co_poll_stream_then_flush",
    [.pollNext, .consumerWait, .flushPoll, .releaseInitialGuard,
      .dropBorrowedNext, .progressPoll, .streamWriteReturned, .progressFlush]⟩

def flushFirstCase : Case :=
  ⟨"fifo_flush_waiter_first",
    [.consumerWait, .flushPoll, .pollNext, .releaseInitialGuard, .flushOnlyPoll,
      .dropBorrowedNext]⟩

def cases : List Case := [borrowedDropCase, generatorDropCase, copollCase, flushFirstCase]

def finalState (c : Case) : State := run waiting c.events

def borrowedDropStalls : State := finalState borrowedDropCase

def generatorDropUnblocks : State := finalState generatorDropCase

def copollUnblocks : State := finalState copollCase

def eventName : Event → String
  | .pollNext => "poll_next"
  | .consumerWait => "consumer_wait"
  | .dropBorrowedNext => "drop_borrowed_next"
  | .releaseInitialGuard => "release_initial_guard"
  | .flushPoll => "flush_poll"
  | .progressPoll => "progress_poll"
  | .progressFlush => "progress_flush"
  | .streamWriteReturned => "stream_write_returned"
  | .flushWriteReturned => "flush_write_returned"
  | .dropGenerator => "drop_generator"
  | .flushOnlyPoll => "flush_only_poll"

def waiterName : Waiter → String
  | .idle => "idle"
  | .queued => "queued"
  | .reserved => "reserved"
  | .acquired => "acquired"
  | .cancelled => "cancelled"

def ownerName : Owner → String
  | .stream => "stream"
  | .flush => "flush"

def ownerOperation (s : State) : String :=
  if s.initialGuardHeld then "initial_guard"
  else if s.streamWaiter == .acquired then "stream"
  else if s.flushWaiter == .acquired then "flush"
  else "none"

def caseJson (c : Case) : String :=
  let final := finalState c
  "{\"name\":" ++ jsonString c.name ++
  ",\"events\":" ++ jsonStringArray (c.events.map eventName) ++
  ",\"stream_waiter\":" ++ jsonString (waiterName final.streamWaiter) ++
  ",\"flush_waiter\":" ++ jsonString (waiterName final.flushWaiter) ++
  ",\"queue_order\":" ++ jsonStringArray (final.queue.map ownerName) ++
  ",\"gate_held\":" ++ (if final.gateHeld then "true" else "false") ++
  ",\"initial_guard_held\":" ++ (if final.initialGuardHeld then "true" else "false") ++
  ",\"owner_operation\":" ++ jsonString (ownerOperation final) ++
  ",\"stream_acquired\":" ++ (if final.streamAcquired then "true" else "false") ++
  ",\"flush_acquire_ready\":" ++ (if final.flushWaiter == .acquired then "true" else "false") ++
  ",\"buffered_item_count\":" ++ toString (if final.bufferedItem then 1 else 0) ++
  ",\"generator_dropped\":" ++ (if final.generatorAlive then "false" else "true") ++
  ",\"generator_alive\":" ++ (if final.generatorAlive then "true" else "false") ++
  ",\"borrowed_next\":" ++ (if final.borrowedNext then "true" else "false") ++
  ",\"consumer_waiting\":" ++ (if final.consumerWaiting then "true" else "false") ++
  ",\"buffered_item\":" ++ (if final.bufferedItem then "true" else "false") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

theorem borrowed_next_drop_preserves_waiter :
    borrowedDropStalls.streamWaiter = .reserved ∧
      borrowedDropStalls.flushWaiter = .queued ∧
      borrowedDropStalls.generatorAlive = true := by
  decide

theorem generator_drop_cancellation_allows_flush_acquire :
    generatorDropUnblocks.streamWaiter = .cancelled ∧
      generatorDropUnblocks.flushWaiter = .acquired := by
  decide

theorem copoll_services_stream_before_flush :
    copollUnblocks.streamWaiter = .idle ∧
      copollUnblocks.flushWaiter = .acquired ∧
      copollUnblocks.bufferedItem = true ∧
      copollUnblocks.streamAcquired = true := by
  decide

theorem copoll_buffers_at_most_one_item :
    step copollUnblocks .progressPoll = copollUnblocks := by
  decide

theorem fifo_case_uses_modelled_queue :
    let final := finalState flushFirstCase
    final.flushWaiter = .acquired ∧ final.streamWaiter = .queued ∧
      final.queue = [.stream] := by
  decide

end Conformance.StorageWriteGateStreamWaiter

def main : IO Unit := IO.println Conformance.StorageWriteGateStreamWaiter.casesJson
