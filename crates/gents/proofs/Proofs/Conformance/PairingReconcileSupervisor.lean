import Proofs.P2P.PairingReconcile.Supervisor
import Proofs.Conformance.Family

namespace Conformance.PairingReconcileSupervisor

open Lean PairingReconcile.Supervisor

structure ShutdownInput where
  name : String
  supervisor : String
  workClass : String
  boundary : String
  cancelledDuringAdminWait : Bool
  deriving ToJson

structure ShutdownOutput where
  perAdminCallTimeoutMs : Nat
  cancellationObservedInsideSweep : Bool
  currentAdminFutureDropped : Bool
  remainingPeersSkipped : Bool
  shutdownJoinBounded : Bool
  deriving ToJson

@[conformance_family]
def shutdownRace : Family ShutdownInput ShutdownOutput where
  inputs :=
    [ ("shutdown_preempts_in_flight_pairing_reconcile_sweep", true)
    , ("uncancelled_pairing_reconcile_sweep_finishes_every_peer", false) ].map
      fun (name, cancelled) =>
        { name, supervisor := "pairingReconciler", workClass := "p2pReconcileSweep"
          boundary := "pairingReconcileSupervisorBoundary", cancelledDuringAdminWait := cancelled }
  model := fun i =>
    let o := race i.cancelledDuringAdminWait
    { perAdminCallTimeoutMs := PairingReconcile.Supervisor.perAdminCallTimeoutMs
      cancellationObservedInsideSweep := o.cancellationObservedInsideSweep
      currentAdminFutureDropped := o.currentAdminFutureDropped
      remainingPeersSkipped := o.remainingPeersSkipped
      shutdownJoinBounded := o.shutdownJoinBounded }

structure RetryInput where
  name : String
  supervisor : String
  workClass : String
  boundary : String
  failureScope : String
  initialFailures : Nat
  deriving ToJson

structure RetryOutput where
  failureTerminal : Bool
  retryTrigger : Option String
  cancellationPrioritized : Bool
  convergenceRetried : Bool
  enumerationAttempts : Nat
  deriving ToJson

@[conformance_family]
def sweepRetry : Family RetryInput RetryOutput where
  inputs :=
    [ ("initial_top_level_sweep_failure_retries_without_terminating_reconciler", 1)
    , ("initial_top_level_sweep_success_converges_without_retry", 0) ].map
      fun (name, initialFailures) =>
        { name, supervisor := "pairingReconciler", workClass := "p2pReconcileSweep"
          boundary := "pairingReconcileSupervisorBoundary"
          failureScope := "topLevelSweepEnumeration", initialFailures }
  model := fun i =>
    let o := PairingReconcile.Supervisor.retry i.initialFailures
    { failureTerminal := o.failureTerminal
      retryTrigger := o.retryTrigger.map RetryTrigger.toWire
      cancellationPrioritized := o.cancellationPrioritized
      convergenceRetried := o.convergenceRetried
      enumerationAttempts := o.enumerationAttempts }

example : perAdminCallTimeoutMs = 10000 := rfl

example : race true =
    { cancellationObservedInsideSweep := true, currentAdminFutureDropped := true
      remainingPeersSkipped := true, shutdownJoinBounded := true } := rfl

example : PairingReconcile.Supervisor.retry 1 =
    { failureTerminal := false, retryTrigger := some .immediateFirstIntervalTick
      cancellationPrioritized := true, convergenceRetried := true
      enumerationAttempts := 2 } := rfl

end Conformance.PairingReconcileSupervisor
