import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.ContractCases.SessionRecovery

/-!
# Session-Recovery Conformance Machine
-/

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

end Conformance.Contracts
