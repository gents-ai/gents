import Proofs.RuntimeReconcile
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.ContractCases.Runtime

namespace Conformance.Contracts

open Conformance.ContractCases

def runtimeReconcileStates : List ReconcilePhase :=
  [ .idle, .debouncing, .resolving, .diffing, .applying ]

def runtimeReconcileStateNames : List String :=
  runtimeReconcileStates.map ReconcilePhase.toDefraDB

def runtimeAckedChanged : RuntimeState :=
  { runtimeBoot with ackedResolved := some runtimeResolvedB }

def runtimeDebouncingChanged : RuntimeState :=
  { runtimeAckedChanged with
    phase := .debouncing
  , observedResolved := some runtimeResolvedB
  }

def runtimeResolvingChanged : RuntimeState :=
  { runtimeDebouncingChanged with phase := .resolving }

def runtimeDiffingNoop : RuntimeState :=
  { runtimeBoot with
    phase := .diffing
  , observedResolved := some runtimeResolvedA
  , pendingResolved := some runtimeResolvedA
  }

def runtimeDiffingChanged : RuntimeState :=
  { runtimeResolvingChanged with
    phase := .diffing
  , pendingResolved := some runtimeResolvedB
  }

def runtimeReconcileSamples : List RuntimeState :=
  [ runtimeBoot
  , runtimeAckedChanged
  , runtimeDebouncingChanged
  , runtimeResolvingChanged
  , runtimeDiffingNoop
  , runtimeDiffingChanged
  , runtimeApplyingChanged
  , runtimePublishedBeforeRouter
  , runtimeRouterObserved
  , runtimeWithInFlight
  ]

def runtimeReconcileActions : List (String × RuntimeState.Action) :=
  [ ("ackWrite", .ackWrite runtimeResolvedB)
  , ("observeDoc", .observeDoc runtimeResolvedB)
  , ("startResolve", .startResolve)
  , ("resolveVisible", .resolveVisible runtimeResolvedB)
  , ("diffNoop", .diffNoop runtimeResolvedA)
  , ("beginApply", .beginApply runtimeResolvedB)
  , ("publish", .publish runtimeResolvedB)
  , ("applyFailed", .applyFailed)
  , ("routerObserve", .routerObserve .ready)
  , ("acceptRequest", .acceptRequest .ready 100 500)
  , ("finishRequest", .finishRequest 500)
  , ("retireGeneration", .retireGeneration 1)
  ]

def runtimeReconcileMachine : StateMachineContract :=
  machineContract
    "RuntimeReconcile"
    runtimeReconcileStateNames
    []
    (actionNames runtimeReconcileActions)
    (transitionPairsFromSamples
      runtimeReconcileSamples
      runtimeReconcileActions
      RuntimeState.step?
      (fun state => state.phase.toDefraDB))

end Conformance.Contracts
