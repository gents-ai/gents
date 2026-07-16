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
  requestIsWrapup : Bool
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
  | .abandonWrapup => "abandon_wrapup"

def goalDecisionCases : List GoalDecisionCase :=
  [ { name := "completed_active_continues", status := .active, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "busy_session_suppresses", status := .active, terminal := .completed,
      sessionIdle := false, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "existing_child_is_exactly_once", status := .active, terminal := .completed,
      sessionIdle := true, childExists := true, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "interrupt_pauses", status := .active, terminal := .interrupted,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "supersede_pauses", status := .active, terminal := .superseded,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "first_infrastructure_failure_retries", status := .active, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "second_infrastructure_failure_retries", status := .active, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 1,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "infrastructure_failure_pauses_at_bound", status := .active, terminal := .dead,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 2,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "no_activity_pauses", status := .active, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := false,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "budget_requests_wrapup", status := .active, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "latched_wrapup_resumes_after_crash", status := .budgetLimited, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := true, wrapupCompleted := false }
  , { name := "completed_wrapup_does_not_repeat", status := .budgetLimited, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := true, infrastructureRetries := 0,
      wrapupRequested := true, wrapupCompleted := false }
  , { name := "failed_wrapup_retries", status := .budgetLimited, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := true, infrastructureRetries := 0,
      wrapupRequested := true, wrapupCompleted := false }
  , { name := "failed_wrapup_abandons_at_bound", status := .budgetLimited, terminal := .dead,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := true, infrastructureRetries := 2,
      wrapupRequested := true, wrapupCompleted := false }
  , { name := "paused_is_inactive", status := .paused, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "blocked_is_inactive", status := .blocked, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "usage_limited_is_inactive", status := .usageLimited, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "complete_is_inactive", status := .complete, terminal := .completed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  ]

def goalDecisionCaseJson (w : GoalDecisionCase) : String :=
  let decision := Goals.decide w.status w.terminal w.sessionIdle w.childExists
    w.budgetReached w.hasActivity w.requestIsWrapup w.infrastructureRetries
    w.wrapupRequested w.wrapupCompleted
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"status\":" ++ jsonString w.status.toDefraDB ++ ","
    ++ "\"terminal\":" ++ jsonString (terminalName w.terminal) ++ ","
    ++ "\"session_idle\":" ++ boolString w.sessionIdle ++ ","
    ++ "\"child_exists\":" ++ boolString w.childExists ++ ","
    ++ "\"budget_reached\":" ++ boolString w.budgetReached ++ ","
    ++ "\"has_activity\":" ++ boolString w.hasActivity ++ ","
    ++ "\"request_is_wrapup\":" ++ boolString w.requestIsWrapup ++ ","
    ++ "\"infrastructure_retries\":" ++ toString w.infrastructureRetries ++ ","
    ++ "\"wrapup_requested\":" ++ boolString w.wrapupRequested ++ ","
    ++ "\"wrapup_completed\":" ++ boolString w.wrapupCompleted ++ ","
    ++ "\"expected_decision\":" ++ jsonString (decisionName decision)
    ++ "}"

def goalDecisionCasesJson : String :=
  jsonArray (goalDecisionCases.map goalDecisionCaseJson)

structure GoalTransitionCase where
  name : String
  pre : Goals.State
  actionName : String
  action : Goals.Action

def goalCaseState (status : Goals.Status) (audits : Nat := 0)
    (requested : Bool := false) (completed : Bool := false) : Goals.State :=
  { status := status, blockedAudits := audits,
    wrapupRequested := requested, wrapupCompleted := completed }

def goalTransitionCases : List GoalTransitionCase :=
  [ { name := "active_pause", pre := goalCaseState .active,
      actionName := "pause", action := .pause }
  , { name := "budget_pause_rejected", pre := goalCaseState .budgetLimited 0 true false,
      actionName := "pause", action := .pause }
  , { name := "blocked_resume_resets_audits", pre := goalCaseState .blocked 3,
      actionName := "resume", action := .resume }
  , { name := "complete_from_active", pre := goalCaseState .active,
      actionName := "complete", action := .complete }
  , { name := "complete_rewrite_rejected", pre := goalCaseState .complete,
      actionName := "complete", action := .complete }
  , { name := "same_request_dedupes", pre := goalCaseState .active 2,
      actionName := "blocked_audit_same_request", action := .blockedAudit .sameRequest }
  , { name := "same_condition_increments", pre := goalCaseState .active 1,
      actionName := "blocked_audit_same_condition", action := .blockedAudit .sameCondition }
  , { name := "third_same_condition_blocks", pre := goalCaseState .active 2,
      actionName := "blocked_audit_same_condition", action := .blockedAudit .sameCondition }
  , { name := "new_condition_resets", pre := goalCaseState .active 2,
      actionName := "blocked_audit_new_condition", action := .blockedAudit .newCondition }
  , { name := "budget_blocked_audit_rejected", pre := goalCaseState .budgetLimited 2 true false,
      actionName := "blocked_audit_same_condition", action := .blockedAudit .sameCondition }
  , { name := "operator_blocks_active", pre := goalCaseState .active,
      actionName := "operator_block", action := .operatorBlock }
  , { name := "usage_limit_stops_active", pre := goalCaseState .active,
      actionName := "usage_limit", action := .usageLimit }
  , { name := "budget_exhaustion_latches_wrapup", pre := goalCaseState .active,
      actionName := "budget_exhausted", action := .budgetExhausted }
  , { name := "wrapup_finishes", pre := goalCaseState .budgetLimited 0 true false,
      actionName := "wrapup_finished", action := .wrapupFinished }
  , { name := "wrapup_abandons_after_retry_bound", pre := goalCaseState .budgetLimited 0 true false,
      actionName := "wrapup_abandoned", action := .wrapupAbandoned }
  , { name := "clean_turn_resets_audits", pre := goalCaseState .active 2,
      actionName := "clean_turn", action := .cleanTurn }
  , { name := "clean_turn_rejected_while_blocked", pre := goalCaseState .blocked 3,
      actionName := "clean_turn", action := .cleanTurn }
  ]

def goalTransitionCaseJson (w : GoalTransitionCase) : String :=
  let result := Goals.step? w.pre w.action
  let post := result.getD w.pre
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"pre_status\":" ++ jsonString w.pre.status.toDefraDB ++ ","
    ++ "\"pre_blocked_audits\":" ++ toString w.pre.blockedAudits ++ ","
    ++ "\"pre_wrapup_requested\":" ++ boolString w.pre.wrapupRequested ++ ","
    ++ "\"pre_wrapup_completed\":" ++ boolString w.pre.wrapupCompleted ++ ","
    ++ "\"action\":" ++ jsonString w.actionName ++ ","
    ++ "\"accepted\":" ++ boolString result.isSome ++ ","
    ++ "\"expected_status\":" ++ jsonString post.status.toDefraDB ++ ","
    ++ "\"expected_blocked_audits\":" ++ toString post.blockedAudits ++ ","
    ++ "\"expected_wrapup_requested\":" ++ boolString post.wrapupRequested ++ ","
    ++ "\"expected_wrapup_completed\":" ++ boolString post.wrapupCompleted
    ++ "}"

def goalTransitionCasesJson : String :=
  jsonArray (goalTransitionCases.map goalTransitionCaseJson)

end Conformance.Contracts
