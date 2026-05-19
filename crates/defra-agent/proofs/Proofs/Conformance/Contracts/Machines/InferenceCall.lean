import Proofs.InferenceCall
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.ContractCases.Types

/-!
# Inference Call Conformance Machine
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def inferenceCallStates : List InferenceCallState :=
  [ .queued, .running, .cancelled, .completed, .failed ]

def inferenceCallStateNames : List String :=
  inferenceCallStates.map InferenceCallState.toDefraDB

def inferenceCallActions : List (String × InferenceCall.Action) :=
  [ ("start", .start)
  , ("complete", .complete)
  , ("fail", .fail)
  , ("cancel", .cancel)
  ]

def inferenceCallWithState (state : InferenceCallState) : InferenceCall :=
  { callId := 1
  , requestId := 1
  , backend := contractBackend
  , state := state
  }

def inferenceCallMachine : StateMachineContract :=
  machineContract
    "InferenceCall"
    inferenceCallStateNames
    (terminalNames inferenceCallStates InferenceCallState.toDefraDB)
    (actionNames inferenceCallActions)
    (transitionPairsFromSamples
      (inferenceCallStates.map inferenceCallWithState)
      inferenceCallActions
      InferenceCall.step?
      (fun call => call.state.toDefraDB))

end Conformance.Contracts
