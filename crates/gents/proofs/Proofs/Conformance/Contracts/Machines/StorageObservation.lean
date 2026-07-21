import Proofs.StorageObservation
import Proofs.Conformance.ContractTypes

/-!
# Storage Observation Conformance Machine
-/

namespace Conformance.Contracts

def storageObservationStates : List StorageObservation :=
  [ .noMutation
  , .inFlight
  , .successAcknowledged
  , .mutationFailed
  , .staleObserved
  , .readVisible
  , .lostAcknowledged
  ]

def storageObservationStateNames : List String :=
  storageObservationStates.map StorageObservation.toContract

def storageObservationActions : List (String × StorageObservation.Action) :=
  [ ("beginMutation", .beginMutation)
  , ("mutationSuccess", .mutationSuccess)
  , ("mutationFailure", .mutationFailure)
  , ("staleRead", .staleRead)
  , ("staleEvent", .staleEvent)
  , ("readYourWrites", .readYourWrites)
  , ("eventArrives", .eventArrives)
  , ("retryFailClosed", .retryFailClosed)
  , ("acknowledgeLost", .acknowledgeLost)
  ]

def storageObservationMachine
    (domain : String)
    (policy : PersistenceState.FailurePolicy) : StateMachineContract :=
  machineContract
    domain
    storageObservationStateNames
    (terminalNames storageObservationStates StorageObservation.toContract)
    (actionNames storageObservationActions)
    (transitionPairsFromSamples
      storageObservationStates
      storageObservationActions
      (fun state action => StorageObservation.step? policy state action)
      StorageObservation.toContract)

end Conformance.Contracts
