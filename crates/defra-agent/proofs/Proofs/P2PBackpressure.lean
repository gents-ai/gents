import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# P2P Backpressure

Per-node admission *obligation* model for the #630 hub backpressure work.

This is **not** a conformance-fenced refinement of the shipping `p2p` /
`defradb.rs` coordinator. It records the local transition obligations that the
TLA+ hub model (`proofs/tla/P2PBackpressure.tla`) and the live fleet depend on:

* a successful inbound PushLog ack remains backed by either a pending-DAG
  registration or a merge;
* pending-DAG registration preserves the configured capacity bound;
* a timeout/failure transition of an in-flight peer **strictly** frees its
  outbound worker slot.

Pending entries are keyed by peer here (and in the TLA model) as a one-wave
abstraction; production capacity is keyed by DAG root / CID. Multi-wave load,
rate limiting, Bitswap stall lifetime, and gossip send-loop health are outside
this model — see `proofs/tla/README.md`.
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
    (_hCapacity : s.inFlight.card < s.workers)
    (_hNotInFlight : peer ∉ s.inFlight) : OutboundState :=
  { s with inFlight := insert peer s.inFlight }

/-- Success path: peer must be in-flight; frees the worker slot. -/
def deliverPush
    (s : OutboundState)
    (peer : PeerId)
    (_hInFlight : peer ∈ s.inFlight) : OutboundState :=
  { s with
    inFlight := s.inFlight.erase peer
    delivered := insert peer s.delivered }

/-- Timeout/failure path: peer must be in-flight; frees the worker slot. -/
def timeoutPush
    (s : OutboundState)
    (peer : PeerId)
    (_hInFlight : peer ∈ s.inFlight) : OutboundState :=
  { s with
    inFlight := s.inFlight.erase peer
    failed := insert peer s.failed }

theorem startPush_slotsBounded
    (s : OutboundState)
    (peer : PeerId)
    (hCapacity : s.inFlight.card < s.workers)
    (hNotInFlight : peer ∉ s.inFlight) :
    pushSlotsBounded (startPush s peer hCapacity hNotInFlight) := by
  unfold pushSlotsBounded startPush
  rw [Finset.card_insert_of_not_mem hNotInFlight]
  exact hCapacity

theorem timeoutPush_strictly_releases_slot
    (s : OutboundState)
    (peer : PeerId)
    (hInFlight : peer ∈ s.inFlight) :
    (timeoutPush s peer hInFlight).inFlight.card + 1 = s.inFlight.card := by
  unfold timeoutPush
  exact Finset.card_erase_add_one hInFlight

theorem deliverPush_strictly_releases_slot
    (s : OutboundState)
    (peer : PeerId)
    (hInFlight : peer ∈ s.inFlight) :
    (deliverPush s peer hInFlight).inFlight.card + 1 = s.inFlight.card := by
  unfold deliverPush
  exact Finset.card_erase_add_one hInFlight

theorem timeoutPush_slotsBounded
    (s : OutboundState)
    (peer : PeerId)
    (hInFlight : peer ∈ s.inFlight)
    (h : pushSlotsBounded s) :
    pushSlotsBounded (timeoutPush s peer hInFlight) := by
  unfold pushSlotsBounded
  have hEq := timeoutPush_strictly_releases_slot s peer hInFlight
  have hLt : (timeoutPush s peer hInFlight).inFlight.card < s.inFlight.card := by
    omega
  exact Nat.le_trans (Nat.le_of_lt hLt) h

theorem deliverPush_slotsBounded
    (s : OutboundState)
    (peer : PeerId)
    (hInFlight : peer ∈ s.inFlight)
    (h : pushSlotsBounded s) :
    pushSlotsBounded (deliverPush s peer hInFlight) := by
  unfold pushSlotsBounded
  have hEq := deliverPush_strictly_releases_slot s peer hInFlight
  have hLt : (deliverPush s peer hInFlight).inFlight.card < s.inFlight.card := by
    omega
  exact Nat.le_trans (Nat.le_of_lt hLt) h

end P2PBackpressure
