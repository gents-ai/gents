import Proofs.P2P.PairingReconcile.SweepScheduling
import Proofs.Conformance.Family

namespace Conformance.PairingSweepScheduling

open Lean PairingReconcile.SweepScheduling

structure SweepInput where
  stalledPeers : Nat
  readyPeers : Nat
  deriving ToJson

structure SweepObservation where
  maxConcurrentPeerPreparations : Nat
  readyPeerBlocked : Bool
  reconciledPeers : Nat
  deriving ToJson

/-- The sweep scenario's transport stalls one dial behind a barrier, so it
builds one stalled peer beside one ready peer. -/
@[conformance_family]
def sweep : Family SweepInput SweepObservation where
  inputs := [{ stalledPeers := 1, readyPeers := 1 }]
  model := fun i =>
    { maxConcurrentPeerPreparations
      readyPeerBlocked := readyPeerBlocked i.stalledPeers
      reconciledPeers := reconciledPeers i.stalledPeers i.readyPeers }

end Conformance.PairingSweepScheduling
