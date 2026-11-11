import Proofs.Store.StorageWriteGate.StreamWaiter
import Proofs.Conformance.Family

namespace Conformance.StorageWriteGateStreamWaiter
open StorageWriteGate.StreamWaiter

deriving instance Lean.ToJson for StorageWriteGate.StreamWaiter.Event

structure Input where
  name : String
  events : List Event
  deriving Lean.ToJson

def borrowedDropCase : Input :=
  ⟨"borrowed_next_drop_stalls_reserved_stream",
    [.pollNext, .consumerWait, .flushPoll, .releaseInitialGuard,
      .dropBorrowedNext, .flushOnlyPoll]⟩

def generatorDropCase : Input :=
  ⟨"generator_drop_cancels_reservation",
    [.pollNext, .consumerWait, .flushPoll, .releaseInitialGuard,
      .dropBorrowedNext, .dropGenerator, .progressFlush]⟩

def copollCase : Input :=
  ⟨"co_poll_stream_then_flush",
    [.pollNext, .consumerWait, .flushPoll, .releaseInitialGuard,
      .dropBorrowedNext, .progressPoll, .streamWriteReturned, .progressFlush]⟩

def flushFirstCase : Input :=
  ⟨"fifo_flush_waiter_first",
    [.consumerWait, .flushPoll, .pollNext, .releaseInitialGuard, .flushOnlyPoll,
      .dropBorrowedNext]⟩

def cases : List Input := [borrowedDropCase, generatorDropCase, copollCase, flushFirstCase]

def finalState (c : Input) : State := run waiting c.events

def borrowedDropStalls : State := finalState borrowedDropCase

def generatorDropUnblocks : State := finalState generatorDropCase

def copollUnblocks : State := finalState copollCase

def ownerOperation (s : State) : String :=
  if s.initialGuardHeld then "initial_guard"
  else if s.streamWaiter == .acquired then "stream"
  else if s.flushWaiter == .acquired then "flush"
  else "none"

structure Output where
  gateHeld : Bool
  initialGuardHeld : Bool
  ownerOperation : String
  streamAcquired : Bool
  flushAcquireReady : Bool
  bufferedItemCount : Nat
  generatorDropped : Bool
  deriving Lean.ToJson

@[conformance_family]
def schedule : Conformance.Family Input Output where
  inputs := cases
  model := fun i =>
    let final := finalState i
    { gateHeld := final.gateHeld
      initialGuardHeld := final.initialGuardHeld
      ownerOperation := ownerOperation final
      streamAcquired := final.streamAcquired
      flushAcquireReady := final.flushWaiter == .acquired
      bufferedItemCount := if final.bufferedItem then 1 else 0
      generatorDropped := !final.generatorAlive }

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
