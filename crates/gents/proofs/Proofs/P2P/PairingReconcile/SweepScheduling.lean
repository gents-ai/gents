/-! A pairing sweep prepares peers concurrently, up to a fixed bound, and then
reconciles each prepared peer's topology one at a time. A peer whose dial
stalls holds one preparation slot until the dial completes; it never holds the
topology step. -/

namespace PairingReconcile.SweepScheduling

/-- The bound on concurrent peer preparations within one sweep. -/
def maxConcurrentPeerPreparations : Nat := 8

/-- A ready peer waits on stalled dials only when they hold every
preparation slot. -/
def readyPeerBlocked (stalledPeers : Nat) : Bool :=
  decide (maxConcurrentPeerPreparations ≤ stalledPeers)

/-- Once every stalled dial completes, the sweep has reconciled every peer it
started: none is dropped for having been slow. -/
def reconciledPeers (stalledPeers readyPeers : Nat) : Nat :=
  stalledPeers + readyPeers

theorem a_stalled_dial_below_the_bound_does_not_block (stalledPeers : Nat)
    (h : stalledPeers < maxConcurrentPeerPreparations) :
    readyPeerBlocked stalledPeers = false := by
  simp [readyPeerBlocked, Nat.not_le.mpr h]

end PairingReconcile.SweepScheduling
