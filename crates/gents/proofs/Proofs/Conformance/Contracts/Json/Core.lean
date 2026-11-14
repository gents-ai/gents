import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases

namespace Conformance.Contracts

open Conformance.ContractCases

def lifecycleTransitionCaseJson (witness : LifecycleTransitionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"domain\":" ++ jsonString witness.domain ++ ","
    ++ "\"stopped_before_running\":" ++ boolString witness.stoppedBeforeRunning ++ ","
    ++ "\"from\":" ++ jsonString witness.fromState ++ ","
    ++ "\"to\":" ++ jsonString witness.toState ++ ","
    ++ "\"classification\":" ++ jsonString witness.classification ++ ","
    ++ "\"action\":" ++ jsonOptionalString witness.action ++ ","
    ++ "\"boundary\":" ++ jsonOptionalString witness.boundary
    ++ "}"

end Conformance.Contracts
