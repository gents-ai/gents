import Proofs.Mailbox.Executable
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def mailboxStatuses : List Mailbox.Status := Mailbox.allStatuses

def mailboxStatusNames : List String :=
  mailboxStatuses.map Mailbox.Status.toDefraDB

def mailboxActions : List (String × Mailbox.ResolutionAction) :=
  [ ("act", .act "doc")
  , ("dismiss", .dismiss "did:key:owner")
  , ("expire", .expire true)
  ]

def mailboxMachine : StateMachineContract :=
  machineContract
    "Mailbox"
    mailboxStatusNames
    ["acted", "dismissed", "expired"]
    (actionNames mailboxActions)
    (transitionPairsFromSamples
      mailboxStatuses
      mailboxActions
      Mailbox.stepStatus?
      Mailbox.Status.toDefraDB)

end Conformance.Contracts
