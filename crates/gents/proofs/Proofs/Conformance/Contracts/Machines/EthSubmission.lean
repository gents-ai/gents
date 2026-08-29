import Proofs.EthSubmission
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def ethSubmissionActions : List (String × EthSubmission.Action) :=
  [ ("broadcast", .broadcast)
  , ("observeSuccess", .observeSuccess)
  , ("observeRevert", .observeRevert)
  ]

def ethSubmissionMachine : StateMachineContract :=
  machineContract
    "EthSubmission"
    (EthSubmission.Status.all.map EthSubmission.Status.toDefraDB)
    (terminalNames EthSubmission.Status.all EthSubmission.Status.toDefraDB)
    (actionNames ethSubmissionActions)
    (transitionPairsFromSamples
      EthSubmission.Status.all
      ethSubmissionActions
      EthSubmission.Status.step?
      EthSubmission.Status.toDefraDB)

end Conformance.Contracts
