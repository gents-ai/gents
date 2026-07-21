import Proofs.Process
import Proofs.Conformance.ContractTypes

/-!
# Process Conformance Machine
-/

namespace Conformance.Contracts

def processStates : List ProcessState :=
  [ .uninitialized, .recovering, .ready, .shuttingDown, .shutdown ]

def processStateNames : List String :=
  processStates.map ProcessState.toDefraDB

def processActions : List (String × ProcessState.Action) :=
  [ ("startupRecover", .startupRecover { hasStuckRequests := true, activeRequestCount := 1 })
  , ("startupClean", .startupClean { hasStuckRequests := false, activeRequestCount := 0 })
  , ("recoveryComplete", .recoveryComplete)
  , ("beginShutdown", .beginShutdown)
  , ("finishShutdown", .finishShutdown 0)
  ]

def processMachine : StateMachineContract :=
  machineContract
    "Process"
    processStateNames
    (terminalNames processStates ProcessState.toDefraDB)
    (actionNames processActions)
    (transitionPairsFromSamples
      processStates
      processActions
      ProcessState.step?
      ProcessState.toDefraDB)

end Conformance.Contracts
