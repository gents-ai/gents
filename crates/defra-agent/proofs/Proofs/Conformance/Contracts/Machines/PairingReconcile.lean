import Proofs.PairingReconcile
import Proofs.Conformance.ContractTypes

/-!
# Pairing-Reconcile Conformance Machine
-/

namespace Conformance.Contracts

def pairingReconcileStates : List PairingReconcile.PairingPhase :=
  [ .idle, .diverged, .converged, .crashed ]

def pairingReconcileStateNames : List String :=
  pairingReconcileStates.map PairingReconcile.PairingPhase.toContract

def pairingReconcileActions : List (String × PairingReconcile.TransitionKind) :=
  [ ("operatorWrite", .operatorWrite)
  , ("operatorDelete", .operatorDelete)
  , ("readFailure", .readFailure)
  , ("dial", .dial)
  , ("peerDisconnected", .peerDisconnected)
  , ("reconcileInstall", .reconcileInstall)
  , ("reconcileTeardown", .reconcileTeardown)
  , ("reconcileInstallReplicator", .reconcileInstallReplicator)
  , ("reconcileTeardownReplicator", .reconcileTeardownReplicator)
  , ("crash", .crash)
  ]

def pairingReconcileMachine : StateMachineContract :=
  machineContract
    "PairingReconcile"
    pairingReconcileStateNames
    []
    (actionNames pairingReconcileActions)
    (transitionPairsFromSamples
      pairingReconcileStates
      pairingReconcileActions
      PairingReconcile.step?
      PairingReconcile.PairingPhase.toContract)

end Conformance.Contracts
