import Proofs.Automation.Goals
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

def goalStatuses : List Goals.Status :=
  [ .active, .paused, .blocked, .usageLimited, .budgetLimited, .complete, .stopped ]

def goalStatusNames : List String :=
  goalStatuses.map Goals.Status.toDefraDB

def goalState (status : Goals.Status)
    (requested : Bool := false) (completed : Bool := false) : Goals.State :=
  { status := status
  , wrapupRequested := requested
  , wrapupCompleted := completed
  }

def goalSamples : List Goals.State :=
  (goalStatuses.map goalState) ++
  [ goalState .budgetLimited true false
  , goalState .budgetLimited true true
  , goalState .paused true true
  ]

def goalActions : List (String × Goals.Action) :=
  [ ("pause", .pause)
  , ("resume", .resume)
  , ("stop", .stop)
  , ("complete", .complete)
  , ("block", .block)
  , ("usage_limit", .usageLimit)
  , ("budget_exhausted", .budgetExhausted)
  , ("wrapup_finished", .wrapupFinished)
  , ("wrapup_abandoned", .wrapupAbandoned)
  ]

def goalMachine : StateMachineContract :=
  machineContract
    "Goal"
    goalStatusNames
    ["complete", "stopped"]
    (actionNames goalActions)
    (transitionPairsFromSamples
      goalSamples
      goalActions
      Goals.step?
      (fun state => state.status.toDefraDB))

end Conformance.Contracts
