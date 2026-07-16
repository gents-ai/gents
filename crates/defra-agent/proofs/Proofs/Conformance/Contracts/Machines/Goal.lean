import Proofs.Goals
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def goalStatuses : List Goals.Status :=
  [ .active, .paused, .blocked, .usageLimited, .budgetLimited, .complete ]

def goalStatusNames : List String :=
  goalStatuses.map Goals.Status.toDefraDB

def goalState (status : Goals.Status) (audits : Nat := 0)
    (requested : Bool := false) (completed : Bool := false) : Goals.State :=
  { status := status
  , blockedAudits := audits
  , wrapupRequested := requested
  , wrapupCompleted := completed
  }

def goalSamples : List Goals.State :=
  (goalStatuses.map goalState) ++
  [ goalState .active 1
  , goalState .active 2
  , goalState .budgetLimited 0 true false
  , goalState .budgetLimited 0 true true
  ]

def goalActions : List (String × Goals.Action) :=
  [ ("pause", .pause)
  , ("resume", .resume)
  , ("complete", .complete)
  , ("blocked_audit_same_request", .blockedAudit .sameRequest)
  , ("blocked_audit_same_condition", .blockedAudit .sameCondition)
  , ("blocked_audit_new_condition", .blockedAudit .newCondition)
  , ("operator_block", .operatorBlock)
  , ("usage_limit", .usageLimit)
  , ("budget_exhausted", .budgetExhausted)
  , ("wrapup_finished", .wrapupFinished)
  , ("wrapup_abandoned", .wrapupAbandoned)
  , ("clean_turn", .cleanTurn)
  ]

def goalMachine : StateMachineContract :=
  machineContract
    "Goal"
    goalStatusNames
    ["complete"]
    (actionNames goalActions)
    (transitionPairsFromSamples
      goalSamples
      goalActions
      Goals.step?
      (fun state => state.status.toDefraDB))

end Conformance.Contracts
