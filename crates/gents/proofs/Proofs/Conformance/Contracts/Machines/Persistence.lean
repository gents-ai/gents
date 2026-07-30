import Proofs.Persistence
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def persistenceStates : List PersistenceState :=
  [ .uncommitted, .committing, .committed, .lost ]

def persistenceStateNames : List String :=
  persistenceStates.map PersistenceState.toDefraDB

def persistenceActions : List (String × PersistenceState.Action) :=
  [ ("flush", .flush)
  , ("writeSuccess", .writeSuccess)
  , ("writeFail", .writeFail)
  , ("accumulate", .accumulate)
  ]

def persistenceMachine
    (domain : String)
    (policy : PersistenceState.FailurePolicy) : StateMachineContract :=
  machineContract
    domain
    persistenceStateNames
    (terminalNames persistenceStates PersistenceState.toDefraDB)
    (actionNames persistenceActions)
    (transitionPairsFromSamples
      persistenceStates
      persistenceActions
      (fun state action => PersistenceState.step? policy state action)
      PersistenceState.toDefraDB)

end Conformance.Contracts
