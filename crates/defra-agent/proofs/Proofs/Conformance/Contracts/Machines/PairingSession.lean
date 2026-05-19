import Proofs.PairingReconcile
import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.ContractCases.SessionRecovery

/-!
# Pairing and Session-Recovery Conformance Machines
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def pairingReconcileStates : List PairingReconcile.PairingPhase :=
  [ .idle, .diverged, .converged, .crashed ]

def pairingReconcileStateNames : List String :=
  pairingReconcileStates.map PairingReconcile.PairingPhase.toContract

def pairingReconcileActions : List (String × PairingReconcile.TransitionKind) :=
  [ ("operatorWrite", .operatorWrite)
  , ("reconcileInstall", .reconcileInstall)
  , ("reconcileTeardown", .reconcileTeardown)
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

def sessionRecoveryLegalTransitions : List TransitionPair :=
  sessionRecoveryCases.filterMap fun witness =>
    if witness.legal then
      some { source := witness.preLatestState, target := witness.postLatestState }
    else
      none

def sessionRecoveryMachine : StateMachineContract :=
  machineContract
    "SessionRecovery"
    requestStateNames
    []
    ["reissueFailed"]
    sessionRecoveryLegalTransitions

end Conformance.Contracts
