import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.ContractCases.SessionRecovery

namespace Conformance.Contracts

open Conformance.ContractCases

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

example : sessionRecoveryMachine.states =
    [ "workspaceBindingPending", "pending", "claimed", "processing", "waitingForBackend"
    , "resumingBackend", "completed", "failed", "superseded", "dead", "interrupted" ] := by
  native_decide

end Conformance.Contracts
