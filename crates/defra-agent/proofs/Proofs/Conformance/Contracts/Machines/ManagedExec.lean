import Proofs.ManagedExec
import Proofs.Conformance.ContractTypes

/-!
# Managed Exec Conformance Machine
-/

namespace Conformance.Contracts

def managedExecStates : List ManagedExecState :=
  ManagedExecState.all

def managedExecStateNames : List String :=
  managedExecStates.map ManagedExecState.toDefraDB

def managedExecActions : List (String × ManagedExecContext.Action) :=
  [ ("spawn", .spawn)
  , ("spawnFailed", .spawnFailed)
  , ("observeExitSuccess", .observeExitSuccess 0)
  , ("observeExitFailure", .observeExitFailure 1)
  , ("deadlineElapsed", .deadlineElapsed)
  , ("cancelRequested", .cancelRequested)
  , ("killObserved", .killObserved)
  , ("reapFailed", .reapFailed)
  ]

def managedExecWithState (state : ManagedExecState) : ManagedExecContext :=
  { state := state
  , deadline := 1
  , now := 2
  , killSignaledAt := none
  , exitCode := none
  }

def managedExecMachine : StateMachineContract :=
  machineContract
    "ManagedExec"
    managedExecStateNames
    (terminalNames managedExecStates ManagedExecState.toDefraDB)
    (actionNames managedExecActions)
    (transitionPairsFromSamples
      (managedExecStates.map managedExecWithState)
      managedExecActions
      ManagedExecContext.step?
      (fun exec => exec.state.toDefraDB))

end Conformance.Contracts
