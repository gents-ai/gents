import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# P2P Backpressure

Per-node admission invariants for the #630 hub backpressure work. The matching
TLA+ model (`proofs/tla/P2PBackpressure.tla`) covers cross-peer fairness and
worker-slot liveness. This Lean model proves the local transition obligations
that make those distributed retries safe:

* a successful inbound PushLog ack remains backed by either a pending-DAG
  registration or a merge;
* pending-DAG registration preserves the configured capacity bound;
* a timeout/failure transition removes the outbound in-flight slot.
-/

namespace P2PBackpressure

abbrev PeerId := String

/-- Local inbound admission state for one hub. -/
structure InboundState where
  pending : Finset PeerId
  merged : Finset PeerId
  acked : Finset PeerId
  nacked : Finset PeerId
  capacity : Nat
  deriving DecidableEq

/-- Every success ack is backed by durable local work. -/
def successAckBacked (s : InboundState) : Prop :=
  ∀ peer, peer ∈ s.acked → peer ∈ s.pending ∨ peer ∈ s.merged

/-- Pending DAG registrations stay within the configured capacity. -/
def pendingBounded (s : InboundState) : Prop :=
  s.pending.card ≤ s.capacity

/-- Complete DAG path: merge and success-ack. -/
def ackMerged (s : InboundState) (peer : PeerId) : InboundState :=
  { s with
    merged := insert peer s.merged
    acked := insert peer s.acked }

/-- Missing-DAG path: register pending and success-ack. -/
def registerPending
    (s : InboundState)
    (peer : PeerId)
    (_hCapacity : s.pending.card < s.capacity) : InboundState :=
  { s with
    pending := insert peer s.pending
    acked := insert peer s.acked }

/-- Capacity/pace refusal path: nack without adding a success ack. -/
def nackAtCapacity (s : InboundState) (peer : PeerId) : InboundState :=
  { s with nacked := insert peer s.nacked }

/-- Forbidden diagnostic path: success-ack without merge or pending tracking. -/
def badAckAtCapacity (s : InboundState) (peer : PeerId) : InboundState :=
  { s with acked := insert peer s.acked }

theorem ackMerged_successAckBacked
    (s : InboundState)
    (peer : PeerId)
    (h : successAckBacked s) :
    successAckBacked (ackMerged s peer) := by
  intro q hq
  simp [successAckBacked, ackMerged] at hq ⊢
  rcases hq with hq | hq
  · exact Or.inr (Or.inl hq)
  · rcases h q hq with hp | hm
    · exact Or.inl hp
    · exact Or.inr (Or.inr hm)

theorem registerPending_successAckBacked
    (s : InboundState)
    (peer : PeerId)
    (hCapacity : s.pending.card < s.capacity)
    (h : successAckBacked s) :
    successAckBacked (registerPending s peer hCapacity) := by
  intro q hq
  simp [successAckBacked, registerPending] at hq ⊢
  rcases hq with hq | hq
  · exact Or.inl (Or.inl hq)
  · rcases h q hq with hp | hm
    · exact Or.inl (Or.inr hp)
    · exact Or.inr hm

theorem nackAtCapacity_successAckBacked
    (s : InboundState)
    (peer : PeerId)
    (h : successAckBacked s) :
    successAckBacked (nackAtCapacity s peer) := by
  intro q hq
  exact h q hq

theorem registerPending_pendingBounded
    (s : InboundState)
    (peer : PeerId)
    (hCapacity : s.pending.card < s.capacity) :
    pendingBounded (registerPending s peer hCapacity) := by
  unfold pendingBounded registerPending
  by_cases hPresent : peer ∈ s.pending
  · simpa [hPresent] using Nat.le_of_lt hCapacity
  · rw [Finset.card_insert_of_not_mem hPresent]
    exact hCapacity

theorem nackAtCapacity_pendingBounded
    (s : InboundState)
    (peer : PeerId)
    (h : pendingBounded s) :
    pendingBounded (nackAtCapacity s peer) := by
  exact h

theorem badAckAtCapacity_can_break_successAckBacked
    (s : InboundState)
    (peer : PeerId)
    (hNotPending : peer ∉ s.pending)
    (hNotMerged : peer ∉ s.merged) :
    ¬ successAckBacked (badAckAtCapacity s peer) := by
  intro h
  have hAck : peer ∈ (badAckAtCapacity s peer).acked := by
    simp [badAckAtCapacity]
  have hBacked := h peer hAck
  simp [badAckAtCapacity, hNotPending, hNotMerged] at hBacked

/-- Local outbound push worker state. -/
structure OutboundState where
  inFlight : Finset PeerId
  delivered : Finset PeerId
  failed : Finset PeerId
  workers : Nat
  deriving DecidableEq

def pushSlotsBounded (s : OutboundState) : Prop :=
  s.inFlight.card ≤ s.workers

def startPush
    (s : OutboundState)
    (peer : PeerId)
    (_hCapacity : s.inFlight.card < s.workers) : OutboundState :=
  { s with inFlight := insert peer s.inFlight }

def deliverPush (s : OutboundState) (peer : PeerId) : OutboundState :=
  { s with
    inFlight := s.inFlight.erase peer
    delivered := insert peer s.delivered }

def timeoutPush (s : OutboundState) (peer : PeerId) : OutboundState :=
  { s with
    inFlight := s.inFlight.erase peer
    failed := insert peer s.failed }

theorem startPush_slotsBounded
    (s : OutboundState)
    (peer : PeerId)
    (hCapacity : s.inFlight.card < s.workers) :
    pushSlotsBounded (startPush s peer hCapacity) := by
  unfold pushSlotsBounded startPush
  by_cases hPresent : peer ∈ s.inFlight
  · simpa [hPresent] using Nat.le_of_lt hCapacity
  · rw [Finset.card_insert_of_not_mem hPresent]
    exact hCapacity

theorem timeoutPush_releases_or_preserves_slot_count
    (s : OutboundState)
  (peer : PeerId) :
    (timeoutPush s peer).inFlight.card ≤ s.inFlight.card := by
  unfold timeoutPush
  exact Finset.card_erase_le

theorem deliverPush_releases_or_preserves_slot_count
    (s : OutboundState)
  (peer : PeerId) :
    (deliverPush s peer).inFlight.card ≤ s.inFlight.card := by
  unfold deliverPush
  exact Finset.card_erase_le

end P2PBackpressure
