import Proofs.Goals
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types

namespace Conformance.Contracts

open Conformance.ContractCases

structure GoalDecisionCase where
  name : String
  status : Goals.Status
  terminal : Goals.RequestTerminal
  sessionIdle : Bool
  childExists : Bool
  budgetReached : Bool
  hasActivity : Bool
  infrastructureRetries : Nat
  wrapupRequested : Bool
  wrapupCompleted : Bool

def terminalName : Goals.RequestTerminal → String
  | .completed => "completed"
  | .failed => "failed"
  | .dead => "dead"
  | .interrupted => "interrupted"
  | .superseded => "superseded"

def decisionName : Goals.Decision → String
  | .none => "none"
  | .continue => "continue"
  | .retry => "retry"
  | .pause => "pause"
  | .wrapup => "wrapup"

def goalDecisionCases : List GoalDecisionCase :=
  [ { name := "completed_active_continues", status := .active, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      infrastructureRetries := 0, wrapupRequested := false, wrapupCompleted := false }
  , { name := "busy_session_suppresses", status := .active, terminal := .completed,
      sessionIdle := false, childExists := false, budgetReached := false, hasActivity := true,
      infrastructureRetries := 0, wrapupRequested := false, wrapupCompleted := false }
  , { name := "existing_child_is_exactly_once", status := .active, terminal := .completed,
      sessionIdle := true, childExists := true, budgetReached := false, hasActivity := true,
      infrastructureRetries := 0, wrapupRequested := false, wrapupCompleted := false }
  , { name := "interrupt_pauses", status := .active, terminal := .interrupted,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      infrastructureRetries := 0, wrapupRequested := false, wrapupCompleted := false }
  , { name := "supersede_pauses", status := .active, terminal := .superseded,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      infrastructureRetries := 0, wrapupRequested := false, wrapupCompleted := false }
  , { name := "infrastructure_failure_retries", status := .active, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      infrastructureRetries := 1, wrapupRequested := false, wrapupCompleted := false }
  , { name := "infrastructure_failure_pauses_at_bound", status := .active, terminal := .dead,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      infrastructureRetries := 2, wrapupRequested := false, wrapupCompleted := false }
  , { name := "no_activity_pauses", status := .active, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := false,
      infrastructureRetries := 0, wrapupRequested := false, wrapupCompleted := false }
  , { name := "budget_requests_wrapup", status := .active, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      infrastructureRetries := 0, wrapupRequested := false, wrapupCompleted := false }
  , { name := "completed_wrapup_does_not_repeat", status := .budgetLimited, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      infrastructureRetries := 0, wrapupRequested := true, wrapupCompleted := true }
  ]

def goalDecisionCaseJson (w : GoalDecisionCase) : String :=
  let decision := Goals.decide w.status w.terminal w.sessionIdle w.childExists
    w.budgetReached w.hasActivity w.infrastructureRetries w.wrapupRequested w.wrapupCompleted
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"status\":" ++ jsonString w.status.toDefraDB ++ ","
    ++ "\"terminal\":" ++ jsonString (terminalName w.terminal) ++ ","
    ++ "\"session_idle\":" ++ boolString w.sessionIdle ++ ","
    ++ "\"child_exists\":" ++ boolString w.childExists ++ ","
    ++ "\"budget_reached\":" ++ boolString w.budgetReached ++ ","
    ++ "\"has_activity\":" ++ boolString w.hasActivity ++ ","
    ++ "\"infrastructure_retries\":" ++ toString w.infrastructureRetries ++ ","
    ++ "\"wrapup_requested\":" ++ boolString w.wrapupRequested ++ ","
    ++ "\"wrapup_completed\":" ++ boolString w.wrapupCompleted ++ ","
    ++ "\"expected_decision\":" ++ jsonString (decisionName decision)
    ++ "}"

def goalDecisionCasesJson : String :=
  jsonArray (goalDecisionCases.map goalDecisionCaseJson)

end Conformance.Contracts
