import Proofs.Goals
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def goalStatusNames : List String :=
  [ Goals.Status.active
  , .paused
  , .blocked
  , .usageLimited
  , .budgetLimited
  , .complete
  ].map Goals.Status.toDefraDB

def goalMachine : StateMachineContract :=
  machineContract
    "Goal"
    goalStatusNames
    ["complete"]
    ["pause", "resume", "complete", "blocked_audit", "budget_exhausted", "wrapup_finished"]
    [ { source := "active", target := "paused" }
    , { source := "active", target := "complete" }
    , { source := "active", target := "active" }
    , { source := "active", target := "blocked" }
    , { source := "active", target := "budget_limited" }
    , { source := "paused", target := "active" }
    , { source := "paused", target := "complete" }
    , { source := "blocked", target := "active" }
    , { source := "blocked", target := "complete" }
    , { source := "usage_limited", target := "active" }
    , { source := "usage_limited", target := "complete" }
    , { source := "budget_limited", target := "budget_limited" }
    , { source := "budget_limited", target := "complete" }
    ]

end Conformance.Contracts
