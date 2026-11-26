import Proofs.Automation.Goals
import Proofs.Automation.GoalProgress
import Proofs.Automation.GoalStopPolicy
import Proofs.Store.Identity.Household
import Proofs.Automation.GoalAutomation
import Proofs.Automation.GoalAutomation.ReadinessGate
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Conformance.Functions.Automation
import Proofs.Conformance.Family

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
  maxInfrastructureRetries : Nat := Goals.maxInfrastructureRetries

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
  , { name := "dead_below_budget_retries", status := .active, terminal := .dead,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "failed_exhausted_requests_wrapup", status := .active, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "dead_exhausted_requests_wrapup", status := .active, terminal := .dead,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "exhausted_failure_at_retry_bound_requests_wrapup", status := .active, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 2,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "exhausted_interrupt_still_pauses", status := .active, terminal := .interrupted,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false }
  , { name := "exhausted_supersede_still_pauses", status := .active, terminal := .superseded,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
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
  , { name := "interrupted_wrapup_pauses_without_retry", status := .budgetLimited,
      terminal := .interrupted, sessionIdle := true, childExists := false,
      budgetReached := true, hasActivity := true, requestIsWrapup := true,
      infrastructureRetries := 0, wrapupRequested := true, wrapupCompleted := false }
  , { name := "failed_wrapup_abandons_at_bound", status := .budgetLimited, terminal := .dead,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := true, infrastructureRetries := 2,
      wrapupRequested := true, wrapupCompleted := false }
  , { name := "configured_retry_limit_retries_past_default", status := .active, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 2,
      wrapupRequested := false, wrapupCompleted := false, maxInfrastructureRetries := 3 }
  , { name := "configured_retry_limit_pauses_at_bound", status := .active, terminal := .dead,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 3,
      wrapupRequested := false, wrapupCompleted := false, maxInfrastructureRetries := 3 }
  , { name := "zero_retry_limit_pauses_first_failure", status := .active, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := false, hasActivity := true,
      requestIsWrapup := false, infrastructureRetries := 0,
      wrapupRequested := false, wrapupCompleted := false, maxInfrastructureRetries := 0 }
  , { name := "zero_retry_limit_abandons_failed_wrapup", status := .budgetLimited, terminal := .failed,
      sessionIdle := true, childExists := false, budgetReached := true, hasActivity := true,
      requestIsWrapup := true, infrastructureRetries := 0,
      wrapupRequested := true, wrapupCompleted := false, maxInfrastructureRetries := 0 }
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
  ] ++ ([Goals.RequestTerminal.completed, .failed, .dead, .interrupted, .superseded].map fun terminal =>
    { name := "stopped_" ++ terminalName terminal ++ "_cannot_continue"
      status := .stopped, terminal
      sessionIdle := true, childExists := false
      budgetReached := decide (terminal = .completed), hasActivity := true
      requestIsWrapup := true, infrastructureRetries := 0
      wrapupRequested := true, wrapupCompleted := false })

deriving instance Lean.ToJson for Goals.RequestTerminal, GoalDecisionCase

@[conformance_family]
def goalDecision : Family GoalDecisionCase Goals.Decision where
  inputs := goalDecisionCases
  model := fun w => Goals.decide w.status w.terminal w.sessionIdle w.childExists
    w.budgetReached w.hasActivity w.requestIsWrapup w.infrastructureRetries
    w.wrapupRequested w.wrapupCompleted w.maxInfrastructureRetries

structure GoalTransitionCase where
  name : String
  pre : Goals.State
  action : Goals.Action

def goalCaseState (status : Goals.Status)
    (requested : Bool := false) (completed : Bool := false) : Goals.State :=
  { status := status, wrapupRequested := requested, wrapupCompleted := completed }

def goalTransitionCases : List GoalTransitionCase :=
  [ { name := "active_pause", pre := goalCaseState .active,
      action := .pause }
  , { name := "budget_wrapup_pauses", pre := goalCaseState .budgetLimited true false,
      action := .pause }
  , { name := "budget_wrapup_resumes_with_obligation",
      pre := goalCaseState .paused true false, action := .resume }
  , { name := "paused_after_wrapup_resumes_with_fresh_budget",
      pre := goalCaseState .paused true true, action := .resume }
  , { name := "finished_wrapup_resumes_with_fresh_budget",
      pre := goalCaseState .budgetLimited true true, action := .resume }
  , { name := "owed_wrapup_cannot_resume", pre := goalCaseState .budgetLimited true false,
      action := .resume }
  , { name := "blocked_resumes", pre := goalCaseState .blocked,
      action := .resume }
  , { name := "complete_from_active", pre := goalCaseState .active,
      action := .complete }
  , { name := "complete_rewrite_rejected", pre := goalCaseState .complete,
      action := .complete }
  , { name := "active_stop", pre := goalCaseState .active,
      action := .stop }
  , { name := "paused_stop", pre := goalCaseState .paused,
      action := .stop }
  , { name := "blocked_stop", pre := goalCaseState .blocked,
      action := .stop }
  , { name := "usage_limited_stop", pre := goalCaseState .usageLimited,
      action := .stop }
  , { name := "budget_stop_preserves_wrapup_history", pre := goalCaseState .budgetLimited true false,
      action := .stop }
  , { name := "complete_stop_rejected", pre := goalCaseState .complete,
      action := .stop }
  , { name := "stopped_stop_rejected", pre := goalCaseState .stopped,
      action := .stop }
  , { name := "stopped_cannot_complete", pre := goalCaseState .stopped,
      action := .complete }
  , { name := "stopped_cannot_resume", pre := goalCaseState .stopped,
      action := .resume }
  , { name := "one_report_blocks_active", pre := goalCaseState .active,
      action := .block }
  , { name := "paused_block_rejected", pre := goalCaseState .paused,
      action := .block }
  , { name := "blocked_block_rejected", pre := goalCaseState .blocked,
      action := .block }
  , { name := "budget_block_rejected", pre := goalCaseState .budgetLimited true false,
      action := .block }
  , { name := "usage_limit_stops_active", pre := goalCaseState .active,
      action := .usageLimit }
  , { name := "budget_exhaustion_latches_wrapup", pre := goalCaseState .active,
      action := .budgetExhausted }
  , { name := "budget_exhaustion_after_fresh_resume", pre := goalCaseState .active,
      action := .budgetExhausted }
  , { name := "wrapup_finishes", pre := goalCaseState .budgetLimited true false,
      action := .wrapupFinished }
  , { name := "wrapup_abandons_after_retry_bound", pre := goalCaseState .budgetLimited true false,
      action := .wrapupAbandoned }
  ]

inductive GoalActionWire where
  | pause
  | resume
  | stop
  | complete
  | block
  | usageLimit
  | budgetExhausted
  | wrapupFinished
  | wrapupAbandoned
  deriving Lean.ToJson

def GoalActionWire.ofModel : Goals.Action → GoalActionWire
  | .pause => .pause
  | .resume => .resume
  | .stop => .stop
  | .complete => .complete
  | .block => .block
  | .usageLimit => .usageLimit
  | .budgetExhausted => .budgetExhausted
  | .wrapupFinished => .wrapupFinished
  | .wrapupAbandoned => .wrapupAbandoned

def GoalActionWire.toModel : GoalActionWire → Goals.Action
  | .pause => .pause
  | .resume => .resume
  | .stop => .stop
  | .complete => .complete
  | .block => .block
  | .usageLimit => .usageLimit
  | .budgetExhausted => .budgetExhausted
  | .wrapupFinished => .wrapupFinished
  | .wrapupAbandoned => .wrapupAbandoned

structure GoalTransitionInput where
  name : String
  preStatus : Goals.Status
  preWrapupRequested : Bool
  preWrapupCompleted : Bool
  action : GoalActionWire
  deriving Lean.ToJson

/-- A rejected step leaves the goal as it was, so the observed state after a
rejection is the state before it. -/
structure GoalTransitionOutput where
  accepted : Bool
  status : Goals.Status
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving Lean.ToJson

@[conformance_family]
def goalTransition : Family GoalTransitionInput GoalTransitionOutput where
  inputs := goalTransitionCases.map fun w =>
    { name := w.name, preStatus := w.pre.status
    , preWrapupRequested := w.pre.wrapupRequested, preWrapupCompleted := w.pre.wrapupCompleted
    , action := GoalActionWire.ofModel w.action }
  model := fun i =>
    let pre : Goals.State :=
      { status := i.preStatus
      , wrapupRequested := i.preWrapupRequested, wrapupCompleted := i.preWrapupCompleted }
    let result := Goals.step? pre i.action.toModel
    let post := result.getD pre
    { accepted := result.isSome, status := post.status
    , wrapupRequested := post.wrapupRequested, wrapupCompleted := post.wrapupCompleted }

def goalStatusesAll : List Goals.Status :=
  [.active, .paused, .blocked, .usageLimited, .budgetLimited, .complete, .stopped]

/-- Every Goal state a user's message can meet. -/
def goalMessageStates : List Goals.State :=
  goalStatusesAll.flatMap fun status =>
    [(false, false), (true, false), (true, true), (false, true)].map fun (requested, completed) =>
      goalCaseState status requested completed

structure GoalMessageResumeInput where
  name : String
  status : Goals.Status
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving Lean.ToJson

structure GoalMessageResumeOutput where
  resumed : Bool
  status : Goals.Status
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving Lean.ToJson

/-- `Goals.resumeOnMessage` over every Goal state: a user's message resumes
only a blocked Goal. -/
@[conformance_family]
def goalMessageResume : Family GoalMessageResumeInput GoalMessageResumeOutput where
  inputs := goalMessageStates.map fun pre =>
    { name := pre.status.toDefraDB ++ (if pre.wrapupRequested then "_requested" else "") ++
        (if pre.wrapupCompleted then "_completed" else "")
    , status := pre.status, wrapupRequested := pre.wrapupRequested
    , wrapupCompleted := pre.wrapupCompleted }
  model := fun i =>
    let pre : Goals.State :=
      { status := i.status, wrapupRequested := i.wrapupRequested
      , wrapupCompleted := i.wrapupCompleted }
    let post := (Goals.resumeOnMessage pre).getD pre
    { resumed := (Goals.resumeOnMessage pre).isSome, status := post.status
    , wrapupRequested := post.wrapupRequested, wrapupCompleted := post.wrapupCompleted }

structure GoalBudgetInput where
  name : String
  tokenBudget : Option Nat
  tokensUsed : Nat
  deriving Lean.ToJson

/-- `Goals.budgetReached`: whether a Goal's charged tokens reached its budget. -/
structure GoalBudgetOutput where
  reached : Bool
  deriving Lean.ToJson

@[conformance_family]
def goalBudget : Family GoalBudgetInput GoalBudgetOutput where
  inputs :=
    [ { name := "under_budget", tokenBudget := some 1000, tokensUsed := 999 }
    , { name := "at_budget", tokenBudget := some 1000, tokensUsed := 1000 }
    , { name := "over_budget", tokenBudget := some 1000, tokensUsed := 1001 }
    , { name := "unset_budget_unspent", tokenBudget := none, tokensUsed := 0 }
    , { name := "unset_budget_heavily_spent", tokenBudget := none, tokensUsed := 5000000 } ]
  model := fun i => { reached := Goals.budgetReached i.tokenBudget i.tokensUsed }

structure CallPrintWire where
  tool : String
  arguments : String
  result : String
  deriving Lean.ToJson

def CallPrintWire.model (call : CallPrintWire) : GoalProgress.CallPrint :=
  ⟨call.tool, call.arguments, call.result⟩

structure GoalProgressInput where
  name : String
  streak : Nat
  previous : List CallPrintWire
  calls : List CallPrintWire
  goal_state_changed : Bool
  live_deadline : Option Nat
  now : Nat
  deriving Lean.ToJson

/-- `GoalProgress.step`: the no-progress record a finished goal turn leaves
and whether the Goal pauses. -/
structure GoalProgressOutput where
  streak : Nat
  pause : Bool
  deriving Lean.ToJson

private def call (tool arguments result : String) : CallPrintWire := ⟨tool, arguments, result⟩

@[conformance_family]
def goalProgress : Family GoalProgressInput GoalProgressOutput where
  inputs :=
    let read := call "read_file" "{\"path\":\"a\"}" "d1"
    let reread := call "read_file" "{\"path\":\"a\"}" "d2"
    let other := call "read_file" "{\"path\":\"b\"}" "d1"
    [ { name := "first_turn_with_a_call", streak := 0, previous := [], calls := [read],
        goal_state_changed := false, live_deadline := none, now := 10 }
    , { name := "repeated_call", streak := 0, previous := [read], calls := [read],
        goal_state_changed := false, live_deadline := none, now := 10 }
    , { name := "third_repeat_pauses", streak := 2, previous := [read], calls := [read],
        goal_state_changed := false, live_deadline := none, now := 10 }
    , { name := "no_calls", streak := 1, previous := [read], calls := [],
        goal_state_changed := false, live_deadline := none, now := 10 }
    , { name := "same_call_new_result", streak := 2, previous := [read], calls := [reread],
        goal_state_changed := false, live_deadline := none, now := 10 }
    , { name := "new_arguments", streak := 2, previous := [read], calls := [read, other],
        goal_state_changed := false, live_deadline := none, now := 10 }
    , { name := "goal_state_changed", streak := 2, previous := [read], calls := [read],
        goal_state_changed := true, live_deadline := none, now := 10 }
    , { name := "verified_wait", streak := 2, previous := [read], calls := [read],
        goal_state_changed := false, live_deadline := some 20, now := 10 }
    , { name := "expired_wait", streak := 2, previous := [read], calls := [read],
        goal_state_changed := false, live_deadline := some 10, now := 10 } ]
  model := fun i =>
    let (record, pause) := GoalProgress.step ⟨i.streak, i.previous.map (·.model)⟩
      ⟨i.calls.map (·.model), i.goal_state_changed⟩ i.live_deadline i.now
    { streak := record.streak, pause }

structure GoalProgressResultFieldInput where
  name : String
  key : String
  deriving Lean.ToJson

/-- `GoalProgress.isVolatileResultField`: whether a JSON key in a tool result
varies only by read-time clock and is stripped before no-progress hashing. -/
structure GoalProgressResultFieldOutput where
  volatile : Bool
  deriving Lean.ToJson

@[conformance_family]
def goalProgressResultField : Family GoalProgressResultFieldInput GoalProgressResultFieldOutput where
  inputs :=
    [ { name := "read_at", key := "read_at" }
    , { name := "elapsed_seconds", key := "elapsed_seconds" }
    , { name := "active_time_seconds", key := "active_time_seconds" }
    , { name := "next_pending_retry_in_ms", key := "next_pending_retry_in_ms" }
    , { name := "lease_remaining_days", key := "lease_remaining_days" }
    , { name := "duration_ms", key := "duration_ms" }
    , { name := "elapsed_ms", key := "elapsed_ms" }
    , { name := "hint", key := "hint" }
    , { name := "status", key := "status" }
    , { name := "ok", key := "ok" }
    , { name := "result", key := "result" }
    , { name := "preview", key := "preview" }
    , { name := "available_bytes", key := "available_bytes" }
    , { name := "started_at", key := "started_at" }
    , { name := "completed_at", key := "completed_at" }
    , { name := "terminal_elapsed_seconds", key := "terminal_elapsed_seconds" }
    , { name := "error", key := "error" } ]
  model := fun i => { volatile := GoalProgress.isVolatileResultField i.key }

structure GoalProgressObservationInput where
  name : String
  tool : String
  arguments : String
  turns : List String
  deriving Lean.ToJson

/-- `GoalProgress.canonicalizeResultText` + `GoalProgress.step`: canonicalized
tool results and per-turn no-progress streaks across consecutive raw tool
outputs. -/
structure GoalProgressObservationOutput where
  canonical_results : List String
  streaks : List Nat
  paused : Bool
  deriving Lean.ToJson

private def runRawObservationTurns (tool arguments : String) :
    GoalProgress.Record → List String → List Nat × Bool
  | _, [] => ([], false)
  | record, raw :: rest =>
      let canonical := GoalProgress.canonicalizeResultText raw
      let call : GoalProgress.CallPrint := ⟨tool, arguments, canonical⟩
      let (next, pause) := GoalProgress.step record ⟨[call], false⟩ none 10
      let (tailStreaks, tailPaused) := runRawObservationTurns tool arguments next rest
      (next.streak :: tailStreaks, pause || tailPaused)

@[conformance_family]
def goalProgressObservation : Family GoalProgressObservationInput GoalProgressObservationOutput where
  inputs :=
    let listPoll (readAt : String) (elapsed : Nat) (bytes : Nat) (preview : String) : String :=
      "{\"ok\":true,\"read_at\":\"" ++ readAt ++ "\",\"truncated\":false," ++
      "\"entries\":[{\"tool_call_id\":\"job-1\",\"status\":\"running\"," ++
      "\"elapsed_seconds\":" ++ toString elapsed ++ "," ++
      "\"available_bytes\":" ++ toString bytes ++ "," ++
      "\"preview\":\"" ++ preview ++ "\"}]}"
    let waitPoll (readAt : String) (elapsed : Nat) (bytes : Nat) : String :=
      "{\"ok\":false,\"tool_call_id\":\"job-1\",\"await_mode\":\"background\"," ++
      "\"status\":\"running\",\"started_at\":\"2026-11-25T09:59:56Z\"," ++
      "\"elapsed_seconds\":" ++ toString elapsed ++ "," ++
      "\"available_bytes\":" ++ toString bytes ++ "," ++
      "\"read_at\":\"" ++ readAt ++ "\",\"result\":null," ++
      "\"error\":{\"reason\":\"wait_timeout\",\"failure_class\":\"external\"}}"
    let getGoalPoll (activeSecs tokens : Nat) : String :=
      "{\"objective\":\"monitor build\",\"status\":\"active\",\"read_only\":false," ++
      "\"tokens_used\":" ++ toString tokens ++ ",\"token_budget\":null," ++
      "\"active_time_seconds\":" ++ toString activeSecs ++ "}"
    let execTimeout (durationMs : Nat) (hintSecs : String) (stdout : String) : String :=
      "gents_exec: {\"ok\":false,\"status\":\"timeout\",\"command\":\"sleep 10\"," ++
      "\"exit_code\":null,\"timed_out\":true,\"duration_ms\":" ++ toString durationMs ++
      ",\"timeout_ms\":5000,\"hint\":\"timed out after " ++ hintSecs ++
      "s; stdout and stderr below were captured before it was stopped.\"}\n" ++
      "stdout:\n" ++ stdout ++ "\nstderr:\n(empty)"
    let fsWalk (elapsedMs visited : Nat) (body : String) : String :=
      "gents_fs: {\"ok\":true,\"status\":\"success\",\"tool\":\"list_files\",\"path\":\".\"," ++
      "\"walk\":{\"entries_visited\":" ++ toString visited ++ ",\"elapsed_ms\":" ++
      toString elapsedMs ++ ",\"budget_exhausted\":false,\"stopped_at\":null}}\n" ++
      "entries:\n" ++ body
    let p2pStatus (retryMs leaseDays pendingDags : Nat) : String :=
      "{\"sync\":{\"pending_dags\":" ++ toString pendingDags ++
      ",\"next_pending_retry_in_ms\":" ++ toString retryMs ++ "}," ++
      "\"history\":[{\"request_id\":\"r1\",\"lease_state\":\"active\"," ++
      "\"lease_remaining_days\":" ++ toString leaseDays ++ "}]}"
    [ { name := "list_processes_volatile_clock_pauses"
      , tool := "list_processes"
      , arguments := "{}"
      , turns :=
          [ listPoll "2026-11-25T10:00:00.000000001Z" 4 12 "booting"
          , listPoll "2026-11-25T10:00:15.123456789Z" 19 12 "booting"
          , listPoll "2026-11-25T10:00:30.234567890Z" 34 12 "booting"
          , listPoll "2026-11-25T10:00:45.345678901Z" 49 12 "booting" ] }
    , { name := "list_processes_non_volatile_growth_resets_streak"
      , tool := "list_processes"
      , arguments := "{}"
      , turns :=
          [ listPoll "2026-11-25T10:00:00Z" 4 12 "booting"
          , listPoll "2026-11-25T10:00:15Z" 19 12 "booting"
          , listPoll "2026-11-25T10:00:30Z" 34 12 "booting"
          , listPoll "2026-11-25T10:00:45Z" 49 48 "ready" ] }
    , { name := "wait_process_volatile_fields_pause"
      , tool := "wait_process"
      , arguments := "{\"tool_call_id\":\"job-1\"}"
      , turns :=
          [ waitPoll "2026-11-25T10:00:00Z" 4 0
          , waitPoll "2026-11-25T10:00:05Z" 9 0
          , waitPoll "2026-11-25T10:00:10Z" 14 0
          , waitPoll "2026-11-25T10:00:15Z" 19 0 ] }
    , { name := "get_goal_volatile_active_time_pauses"
      , tool := "get_goal"
      , arguments := "{}"
      , turns :=
          [ getGoalPoll 10 317
          , getGoalPoll 25 317
          , getGoalPoll 40 317
          , getGoalPoll 55 317 ] }
    , { name := "bash_exec_envelope_volatile_duration_and_hint_pauses"
      , tool := "bash"
      , arguments := "{\"args\":[\"10\"],\"command\":\"sleep\"}"
      , turns :=
          [ execTimeout 5001 "5.0" "waiting"
          , execTimeout 5017 "5.0" "waiting"
          , execTimeout 5054 "5.1" "waiting"
          , execTimeout 5189 "5.2" "waiting" ] }
    , { name := "bash_exec_envelope_stdout_change_resets_streak"
      , tool := "bash"
      , arguments := "{\"args\":[\"10\"],\"command\":\"sleep\"}"
      , turns :=
          [ execTimeout 5001 "5.0" "waiting"
          , execTimeout 5017 "5.0" "waiting"
          , execTimeout 5054 "5.1" "waiting"
          , execTimeout 5189 "5.2" "waiting\nstep 2" ] }
    , { name := "fs_envelope_volatile_walk_elapsed_ms_pauses"
      , tool := "list_files"
      , arguments := "{\"path\":\".\"}"
      , turns :=
          [ fsWalk 4 3 "file a.rs"
          , fsWalk 17 3 "file a.rs"
          , fsWalk 9 3 "file a.rs"
          , fsWalk 23 3 "file a.rs" ] }
    , { name := "fs_envelope_non_volatile_walk_change_resets_streak"
      , tool := "list_files"
      , arguments := "{\"path\":\".\"}"
      , turns :=
          [ fsWalk 4 3 "file a.rs"
          , fsWalk 17 3 "file a.rs"
          , fsWalk 9 3 "file a.rs"
          , fsWalk 23 4 "file a.rs\nfile b.rs" ] }
    , { name := "p2p_sync_and_lease_countdowns_pause"
      , tool := "p2p"
      , arguments := "{\"action\":\"status\"}"
      , turns :=
          [ p2pStatus 1500 29 0
          , p2pStatus 1200 29 0
          , p2pStatus 900 28 0
          , p2pStatus 600 28 0 ] }
    , { name := "reordered_json_keys_with_volatile_diff_pause"
      , tool := "list_processes"
      , arguments := "{}"
      , turns :=
          [ "{\"z\":2,\"read_at\":\"t1\",\"a\":1}"
          , "{\"a\":1,\"read_at\":\"t2\",\"z\":2}"
          , "{\"read_at\":\"t3\",\"z\":2,\"a\":1}"
          , "{\"z\":2,\"a\":1,\"read_at\":\"t4\"}" ] }
    , { name := "non_command_hint_change_is_progress"
      , tool := "lsp"
      , arguments := "{\"action\":\"diagnostics\"}"
      , turns :=
          [ "{\"ok\":true,\"hint\":\"check line 1\"}"
          , "{\"ok\":true,\"hint\":\"check line 2\"}"
          , "{\"ok\":true,\"hint\":\"check line 3\"}" ] }
    , { name := "terminal_timestamps_and_durations_reset_streak"
      , tool := "list_processes"
      , arguments := "{}"
      , turns :=
          [ "{\"status\":\"completed\",\"started_at\":\"2026-11-25T10:00:00Z\",\"completed_at\":\"2026-11-25T10:00:01Z\",\"terminal_elapsed_seconds\":1}"
          , "{\"status\":\"completed\",\"started_at\":\"2026-11-25T10:00:00Z\",\"completed_at\":\"2026-11-25T10:00:02Z\",\"terminal_elapsed_seconds\":2}" ] }
    , { name := "plain_text_repeats_pause_and_change_resets"
      , tool := "grep"
      , arguments := "{\"pattern\":\"TODO\"}"
      , turns :=
          [ "no matches"
          , "no matches"
          , "no matches"
          , "1 match" ] }
    , { name := "malformed_envelope_header_stays_verbatim"
      , tool := "bash"
      , arguments := "{}"
      , turns :=
          [ "gents_exec: {unparseable\nstdout:\nok"
          , "gents_exec: {unparseable\nstdout:\nchanged" ] } ]
  model := fun i =>
    let (streaks, paused) := runRawObservationTurns i.tool i.arguments GoalProgress.Record.fresh i.turns
    { canonical_results := i.turns.map GoalProgress.canonicalizeResultText
    , streaks
    , paused }

def onBlockedName : GoalStopPolicy.OnBlocked → String
  | .stopFirst => "stop_first"
  | .confirmOnce => "confirm_once"
  | .keepTrying => "keep_trying"

def outcomeName : GoalStopPolicy.Outcome → String
  | .pause => "pause"
  | .confirm => "confirm"
  | .continue => "continue"

structure GoalOnBlockedInput where
  name : String
  on_blocked : String
  confirming : Bool
  deriving Lean.ToJson

/-- `GoalStopPolicy.onBlockedReport` over every setting. -/
structure GoalOnBlockedOutput where
  outcome : String
  deriving Lean.ToJson

@[conformance_family]
def goalOnBlocked : Family GoalOnBlockedInput GoalOnBlockedOutput where
  inputs := ([GoalStopPolicy.OnBlocked.stopFirst, .confirmOnce, .keepTrying].flatMap fun policy =>
    [false, true].map fun confirming =>
      { name := onBlockedName policy ++ (if confirming then "_confirming" else "_first"),
        on_blocked := onBlockedName policy, confirming })
  model := fun i =>
    let policy := if i.on_blocked = "confirm_once" then GoalStopPolicy.OnBlocked.confirmOnce
      else if i.on_blocked = "keep_trying" then .keepTrying else .stopFirst
    { outcome := outcomeName (GoalStopPolicy.onBlockedReport policy i.confirming) }

structure GoalEffectiveBudgetInput where
  name : String
  goal_budget : Option Nat
  token_ceiling : Option Nat
  turn_ceiling : Option Nat
  lifted : Bool
  tokens_used : Nat
  turns : Nat
  deriving Lean.ToJson

/-- `GoalStopPolicy.effectiveBudget` and `limitReached`: the budget the
harness enforces and whether the Goal reached its limit. -/
structure GoalEffectiveBudgetOutput where
  budget : Option Nat
  limit_reached : Bool
  deriving Lean.ToJson

@[conformance_family]
def goalEffectiveBudget : Family GoalEffectiveBudgetInput GoalEffectiveBudgetOutput where
  inputs :=
    let case (name : String) (goal ceiling turnCeiling : Option Nat) (lifted : Bool)
        (used turns : Nat) : GoalEffectiveBudgetInput :=
      { name, goal_budget := goal, token_ceiling := ceiling, turn_ceiling := turnCeiling,
        lifted, tokens_used := used, turns }
    [ case "neither" none none none false 5000000 40
    , case "goal_only_under" (some 500) none none false 499 1
    , case "goal_only_reached" (some 500) none none false 500 1
    , case "ceiling_only_reached" none (some 800) none false 800 1
    , case "goal_below_ceiling" (some 500) (some 800) none false 600 1
    , case "goal_above_ceiling" (some 900) (some 800) none false 850 1
    , case "lifted_ceiling" (some 900) (some 800) none true 850 1
    , case "lifted_without_goal_budget" none (some 800) none true 5000 1
    , case "turn_ceiling_under" none none (some 5) false 0 4
    , case "turn_ceiling_reached" none none (some 5) false 0 5
    , case "lifted_turn_ceiling" none none (some 5) true 0 9 ]
  model := fun i =>
    let policy : GoalStopPolicy.Policy := ⟨.stopFirst, i.token_ceiling, i.turn_ceiling, i.lifted⟩
    { budget := GoalStopPolicy.effectiveBudget policy i.goal_budget
    , limit_reached := GoalStopPolicy.limitReached policy i.goal_budget i.tokens_used i.turns }

structure GoalPolicyAccessInput where
  name : String
  actor : String
  operation : String
  deriving Lean.ToJson

/-- `Identity.Household` over `goal_policies`, in order: who may create and
then update a Goal's stop policy, the working Agent or its user. -/
@[conformance_family]
def goalPolicyAccess : Family GoalPolicyAccessInput Bool where
  inputs :=
    [ { name := "working_agent_creates", actor := "working_agent", operation := "create" }
    , { name := "user_creates", actor := "user", operation := "create" }
    , { name := "working_agent_updates", actor := "working_agent", operation := "update" }
    , { name := "user_updates", actor := "user", operation := "update" } ]
  model := fun i =>
    let actor := if i.actor = "user" then "user" else "worker"
    if i.operation = "create" then
      Identity.Household.gatedCreateAllowed Identity.Household.policy
        Identity.Household.goalPolicyFixture actor "goal_policies" "agents" "worker"
    else
      Identity.Household.allowed 5 Identity.Household.goalPolicyFixture actor "goal_policies"
        "policy" "update"

structure GoalCreateCase where
  name : String
  request : GoalAutomation.CreateRequest
  existing : Option GoalAutomation.Fingerprint

def createRequest (owner session objective : String) (budget : Option Int := none)
    (goalTools : Bool := true) (goalCreate : Bool := true)
    (retries : Option Int := none) :
    GoalAutomation.CreateRequest :=
  { caller := owner, currentSession := session
  , requestedOwner := owner, requestedSession := session
  , objective := objective, objectiveNonempty := !objective.isEmpty
  , tokenBudget := budget
  , maxInfrastructureRetries := retries, goalTools := goalTools, goalCreate := goalCreate }

def goalCreateCases : List GoalCreateCase :=
  let base := createRequest "did:a" "session-a" "ship feature"
  let mismatch : GoalAutomation.Fingerprint :=
    { owner := "did:a", session := "session-a"
    , objective := "different", tokenBudget := none }
  let spaced := createRequest "did:a" "session-a" "  ship feature  "
  let canonical : GoalAutomation.Fingerprint :=
    { owner := "did:a", session := "session-a"
    , objective := "ship feature", tokenBudget := none }
  let pinned := createRequest "did:a" "session-a" "ship feature" (retries := some 4)
  [ { name := "authorized_fresh", request := base, existing := none }
  , { name := "authorized_duplicate_idempotent", request := base,
      existing := some (GoalAutomation.createFingerprint base) }
  , { name := "normalized_duplicate_idempotent", request := spaced,
      existing := some canonical }
  , { name := "duplicate_conflicting_objective", request := base,
      existing := some mismatch }
  , { name := "cross_did_denied",
      request := { base with requestedOwner := "did:b" }, existing := none }
  , { name := "cross_session_denied",
      request := { base with requestedSession := "session-b" }, existing := none }
  , { name := "goal_tools_denied",
      request := { base with goalTools := false }, existing := none }
  , { name := "goal_create_denied",
      request := { base with goalCreate := false }, existing := none }
  , { name := "blank_objective_invalid",
      request := createRequest "did:a" "session-a" "", existing := none }
  , { name := "whitespace_objective_invalid",
      request := createRequest "did:a" "session-a" "   ", existing := none }
  , { name := "unicode_whitespace_objective_invalid",
      request := createRequest "did:a" "session-a" "\u3000\u00A0", existing := none }
  , { name := "unicode_normalized_duplicate_idempotent",
      request := createRequest "did:a" "session-a" "\u3000ship feature\u00A0",
      existing := some canonical }
  , { name := "zero_budget_invalid",
      request := createRequest "did:a" "session-a" "ship" (some 0), existing := none }
  , { name := "negative_budget_invalid",
      request := createRequest "did:a" "session-a" "ship" (some (-1)), existing := none }
  , { name := "maximum_budget_valid",
      request := createRequest "did:a" "session-a" "ship"
        (some GoalAutomation.maxTokenBudget), existing := none }
  , { name := "overflow_budget_invalid",
      request := createRequest "did:a" "session-a" "ship"
        (some (GoalAutomation.maxTokenBudget + 1)), existing := none }
  , { name := "pinned_limits_fresh", request := pinned, existing := none }
  , { name := "pinned_limits_duplicate_idempotent", request := pinned,
      existing := some (GoalAutomation.createFingerprint pinned) }
  , { name := "duplicate_conflicting_limits", request := pinned,
      existing := some canonical }
  , { name := "duplicate_dropping_limits_conflicts", request := base,
      existing := some (GoalAutomation.createFingerprint pinned) }
  , { name := "zero_retry_limit_valid",
      request := createRequest "did:a" "session-a" "ship" (retries := some 0),
      existing := none }
  , { name := "negative_retry_limit_invalid",
      request := createRequest "did:a" "session-a" "ship" (retries := some (-1)),
      existing := none }
  ]

structure TaskGoalPublicationCase where
  name : String
  nodeDid : String
  taskId : String
  fireKey : String
  declaration : GoalAutomation.TaskGoalDeclaration

def taskGoalPublicationCases : List TaskGoalPublicationCase :=
  let nodeDid := "did:key:z-feature-owner"
  let taskId := "feature-implementation"
  let fireKey := "schedule:2026-09-02T12:00:00Z"
  [ { name := "absent_goal_uses_ordinary_publication", nodeDid, taskId, fireKey
      declaration := ⟨none, none, none⟩ }
  , { name := "objective_uses_atomic_goal_publication", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature", none, none⟩ }
  , { name := "positive_budget_uses_atomic_goal_publication", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature", some 1000, none⟩ }
  , { name := "maximum_budget_uses_atomic_goal_publication", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature", some GoalAutomation.maxTokenBudget, none⟩ }
  , { name := "same_task_fire_is_scoped_to_other_node",
      nodeDid := "did:key:z-other-feature-owner", taskId, fireKey
      declaration := ⟨some "implement feature", none, none⟩ }
  , { name := "blank_objective_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨some "", none, none⟩ }
  , { name := "whitespace_objective_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨some "   ", none, none⟩ }
  , { name := "budget_without_objective_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨none, some 1000, none⟩ }
  , { name := "zero_budget_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature", some 0, none⟩ }
  , { name := "negative_budget_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature", some (-1), none⟩ }
  , { name := "overflow_budget_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature",
        some (GoalAutomation.maxTokenBudget + 1), none⟩ }
  , { name := "goal_limits_use_atomic_goal_publication", nodeDid, taskId,
      fireKey := "schedule:2026-09-03T12:00:00Z"
      declaration := ⟨some "implement feature", none, some 4⟩ }
  , { name := "zero_retry_limit_uses_atomic_goal_publication", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature", none, some 0⟩ }
  , { name := "retry_limit_without_objective_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨none, none, some 4⟩ }
  , { name := "negative_retry_limit_cannot_publish", nodeDid, taskId, fireKey
      declaration := ⟨some "implement feature", none, some (-1)⟩ }
  ]

def budgetWire (budget : Option Int) : Option Lean.Json := budget.map Lean.toJson

def budgetOfWire (budget : Option Lean.Json) : Option Int :=
  budget.bind fun value => value.getInt?.toOption

structure GoalFingerprintWire where
  owner : String
  session : String
  objective : String
  tokenBudget : Option Lean.Json
  maxInfrastructureRetries : Option Int
  deriving Lean.ToJson

def GoalFingerprintWire.ofModel (f : GoalAutomation.Fingerprint) : GoalFingerprintWire :=
  { owner := f.owner, session := f.session, objective := f.objective
  , tokenBudget := budgetWire f.tokenBudget
  , maxInfrastructureRetries := f.maxInfrastructureRetries }

def GoalFingerprintWire.toModel (f : GoalFingerprintWire) : GoalAutomation.Fingerprint :=
  { owner := f.owner, session := f.session, objective := f.objective
  , tokenBudget := budgetOfWire f.tokenBudget
  , maxInfrastructureRetries := f.maxInfrastructureRetries }

/-- A token budget is a JSON number: the overflow case carries
`maxTokenBudget + 1`, which no signed 64-bit wire integer holds. -/
structure GoalCreateInput where
  name : String
  caller : String
  currentSession : String
  requestedOwner : String
  requestedSession : String
  objective : String
  objectiveNonempty : Bool
  tokenBudget : Option Lean.Json
  maxInfrastructureRetries : Option Int
  goalTools : Bool
  goalCreate : Bool
  existingFingerprint : Option GoalFingerprintWire
  deriving Lean.ToJson

def GoalCreateInput.ofCase (w : GoalCreateCase) : GoalCreateInput :=
  { name := w.name, caller := w.request.caller, currentSession := w.request.currentSession
  , requestedOwner := w.request.requestedOwner, requestedSession := w.request.requestedSession
  , objective := w.request.objective, objectiveNonempty := w.request.objectiveNonempty
  , tokenBudget := budgetWire w.request.tokenBudget
  , maxInfrastructureRetries := w.request.maxInfrastructureRetries
  , goalTools := w.request.goalTools, goalCreate := w.request.goalCreate
  , existingFingerprint := w.existing.map GoalFingerprintWire.ofModel }

def GoalCreateInput.request (i : GoalCreateInput) : GoalAutomation.CreateRequest :=
  { caller := i.caller, currentSession := i.currentSession
  , requestedOwner := i.requestedOwner, requestedSession := i.requestedSession
  , objective := i.objective, objectiveNonempty := i.objectiveNonempty
  , tokenBudget := budgetOfWire i.tokenBudget
  , maxInfrastructureRetries := i.maxInfrastructureRetries
  , goalTools := i.goalTools, goalCreate := i.goalCreate }

deriving instance Lean.ToJson for GoalAutomation.CreateDisposition

@[conformance_family]
def goalCreate : Family GoalCreateInput GoalAutomation.CreateDisposition where
  inputs := goalCreateCases.map GoalCreateInput.ofCase
  model := fun i => GoalAutomation.decideCreate i.request (i.existingFingerprint.map GoalFingerprintWire.toModel)

/-- The goal hook takes the owner and session from its bound identity and the
existing goal from the store, so the dispatch-replay scenario builds only an
authorized fresh request with no budget. -/
def goalCreateCommittedCases : List GoalCreateCase :=
  goalCreateCases.filter fun c => c.name ∈ ["authorized_fresh"]

@[conformance_family]
def goalCreateCommitted : Family GoalCreateInput GoalAutomation.CreateDisposition where
  inputs := goalCreateCommittedCases.map GoalCreateInput.ofCase
  model := fun i => GoalAutomation.decideCreate i.request (i.existingFingerprint.map GoalFingerprintWire.toModel)

structure TaskGoalDeclarationInput where
  name : String
  goalObjective : Option String
  goalTokenBudget : Option Lean.Json
  goalMaxInfrastructureRetries : Option Int
  deriving Lean.ToJson

structure TaskGoalDeclarationOutput where
  declarationValid : Bool
  deriving Lean.ToJson

@[conformance_family]
def taskGoalDeclaration : Family TaskGoalDeclarationInput TaskGoalDeclarationOutput where
  inputs := taskGoalPublicationCases.map fun w =>
    { name := w.name, goalObjective := w.declaration.goalObjective
    , goalTokenBudget := budgetWire w.declaration.goalTokenBudget
    , goalMaxInfrastructureRetries := w.declaration.goalMaxInfrastructureRetries }
  model := fun i =>
    { declarationValid := GoalAutomation.validTaskGoalDeclaration
        ⟨i.goalObjective, budgetOfWire i.goalTokenBudget, i.goalMaxInfrastructureRetries⟩ }

structure TaskGoalPublicationInput where
  name : String
  nodeDid : String
  taskId : String
  fireKey : String
  goalObjective : Option String
  goalTokenBudget : Option Lean.Json
  goalMaxInfrastructureRetries : Option Int
  deriving Lean.ToJson

def TaskGoalPublicationInput.ofCase (w : TaskGoalPublicationCase) : TaskGoalPublicationInput :=
  { name := w.name, nodeDid := w.nodeDid, taskId := w.taskId, fireKey := w.fireKey
  , goalObjective := w.declaration.goalObjective
  , goalTokenBudget := budgetWire w.declaration.goalTokenBudget
  , goalMaxInfrastructureRetries := w.declaration.goalMaxInfrastructureRetries }

def TaskGoalPublicationInput.publication (i : TaskGoalPublicationInput) :
    GoalAutomation.TaskPublication :=
  GoalAutomation.decideTaskPublication
    ⟨i.goalObjective, budgetOfWire i.goalTokenBudget, i.goalMaxInfrastructureRetries⟩
    i.nodeDid i.taskId i.fireKey

structure TaskGoalPublicationIdentity where
  sessionId : Option String
  requestId : Option String
  retryKey : Option String
  deriving Lean.ToJson

/-- Only an atomic goal-backed publication has a fire identity; the others are
covered by `taskGoalDeclaration`. -/
def taskGoalPublicationIdentityCases : List TaskGoalPublicationCase :=
  taskGoalPublicationCases.filter fun w =>
    (GoalAutomation.decideTaskPublication w.declaration w.nodeDid w.taskId w.fireKey).mode
      == .atomicGoalBacked

@[conformance_family]
def taskGoalPublicationIdentity :
    Family TaskGoalPublicationInput TaskGoalPublicationIdentity where
  inputs := taskGoalPublicationIdentityCases.map TaskGoalPublicationInput.ofCase
  model := fun i =>
    let publication := i.publication
    { sessionId := publication.sessionId, requestId := publication.requestId
    , retryKey := publication.retryKey }

deriving instance Lean.ToJson for GoalAutomation.TaskPublicationMode

structure TaskGoalPublicationMaterialized where
  mode : GoalAutomation.TaskPublicationMode
  published : Bool
  runnableRequest : Bool
  durableGoal : Bool
  deriving Lean.ToJson

/-- The production materializer scenario publishes scheduled goal Tasks on
its own node; it builds the positive-budget and the goal-limits declarations,
whose distinct fires open distinct Goals. -/
def taskGoalPublicationMaterializedCases : List TaskGoalPublicationCase :=
  taskGoalPublicationCases.filter fun w =>
    w.name ∈ ["positive_budget_uses_atomic_goal_publication",
      "goal_limits_use_atomic_goal_publication"]

@[conformance_family]
def taskGoalPublicationMaterialized :
    Family TaskGoalPublicationInput TaskGoalPublicationMaterialized where
  inputs := taskGoalPublicationMaterializedCases.map TaskGoalPublicationInput.ofCase
  model := fun i =>
    let publication := i.publication
    { mode := publication.mode, published := publication.published
    , runnableRequest := publication.runnableRequest, durableGoal := publication.durableGoal }

structure TaskGoalRecoveryCase where
  name : String
  nodeDid : String
  agentDid : String
  taskId : String
  fireKey : String
  request : Option GoalAutomation.TaskGoalRequestBinding
  durableGoalPresent : Bool
  creationClaimPresent : Bool

def taskGoalRecoveryCases : List TaskGoalRecoveryCase :=
  let nodeDid := "did:key:z-feature-owner"
  let agentDid := "feature-engineer"
  let taskId := "feature-implementation"
  let fireKey := "schedule:2026-09-02T12:00:00Z"
  let expected := GoalAutomation.expectedTaskGoalRequestBinding
    nodeDid agentDid taskId fireKey
  [ { name := "request_absent_has_nothing_to_checkpoint", nodeDid, agentDid, taskId, fireKey
      request := none, durableGoalPresent := false, creationClaimPresent := false }
  , { name := "matching_request_with_metadata_recovers", nodeDid, agentDid, taskId, fireKey
      request := some expected, durableGoalPresent := true, creationClaimPresent := true }
  , { name := "matching_request_without_goal_recovers", nodeDid, agentDid, taskId, fireKey
      request := some expected, durableGoalPresent := false, creationClaimPresent := true }
  , { name := "matching_request_without_claim_recovers", nodeDid, agentDid, taskId, fireKey
      request := some expected, durableGoalPresent := true, creationClaimPresent := false }
  , { name := "matching_request_without_goal_or_claim_recovers",
      nodeDid, agentDid, taskId, fireKey
      request := some expected, durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_node_conflicts",
      nodeDid, agentDid, taskId, fireKey
      request := some { expected with nodeDid := "did:key:z-other-owner" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_agent_conflicts", nodeDid, agentDid, taskId, fireKey
      request := some { expected with agentDid := "other-agent" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_session_conflicts", nodeDid, agentDid, taskId, fireKey
      request := some { expected with sessionId := "other-session" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_request_id_conflicts", nodeDid, agentDid, taskId, fireKey
      request := some { expected with requestId := "other-request" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_retry_key_conflicts", nodeDid, agentDid, taskId, fireKey
      request := some { expected with retryKey := "other-retry" }
      durableGoalPresent := false, creationClaimPresent := false }
  ]

structure TaskGoalRecoveryInput where
  name : String
  nodeDid : String
  agentDid : String
  taskId : String
  fireKey : String
  requestPresent : Bool
  observedNodeDid : Option String
  observedAgentDid : Option String
  observedSessionId : Option String
  observedRequestId : Option String
  observedRetryKey : Option String
  durableGoalPresent : Bool
  creationClaimPresent : Bool
  deriving Lean.ToJson

def TaskGoalRecoveryInput.ofCase (w : TaskGoalRecoveryCase) : TaskGoalRecoveryInput :=
  { name := w.name, nodeDid := w.nodeDid, agentDid := w.agentDid, taskId := w.taskId
  , fireKey := w.fireKey, requestPresent := w.request.isSome
  , observedNodeDid := w.request.map (·.nodeDid)
  , observedAgentDid := w.request.map (·.agentDid)
  , observedSessionId := w.request.map (·.sessionId)
  , observedRequestId := w.request.map (·.requestId)
  , observedRetryKey := w.request.map (·.retryKey)
  , durableGoalPresent := w.durableGoalPresent, creationClaimPresent := w.creationClaimPresent }

def TaskGoalRecoveryInput.request (i : TaskGoalRecoveryInput) :
    Option GoalAutomation.TaskGoalRequestBinding :=
  if i.requestPresent then
    some { nodeDid := i.observedNodeDid.getD "", agentDid := i.observedAgentDid.getD ""
         , sessionId := i.observedSessionId.getD "", requestId := i.observedRequestId.getD ""
         , retryKey := i.observedRetryKey.getD "" }
  else none

deriving instance Lean.ToJson for GoalAutomation.TaskFireRecoveryDisposition

structure TaskGoalRecoveryOutput where
  requestBindingMatches : Bool
  disposition : GoalAutomation.TaskFireRecoveryDisposition
  recoveredRequestId : Option String
  checkpointable : Bool
  deriving Lean.ToJson

@[conformance_family]
def taskGoalRecovery : Family TaskGoalRecoveryInput TaskGoalRecoveryOutput where
  inputs := taskGoalRecoveryCases.map TaskGoalRecoveryInput.ofCase
  model := fun i =>
    let expected := GoalAutomation.expectedTaskGoalRequestBinding
      i.nodeDid i.agentDid i.taskId i.fireKey
    let decision := GoalAutomation.decideTaskFireRecovery expected
      ⟨i.request, i.durableGoalPresent, i.creationClaimPresent⟩
    { requestBindingMatches := i.request == some expected
    , disposition := decision.disposition
    , recoveredRequestId := decision.recoveredRequestId
    , checkpointable := decision.checkpointable }

structure GoalSubmissionCase where
  name : String
  state : GoalAutomation.SubmissionState
  action : GoalAutomation.SubmissionAction

structure RuntimeGoalOpeningInput where
  name : String
  actor : String
  signer : String
  selected : String
  targetSession : String
  observedSession : String
  targetAgent : String
  observedAgent : String
  fresh : Bool
  commit : Bool
  deriving Lean.ToJson

deriving instance Lean.ToJson for GoalAutomation.RuntimeGoalPublication

/-- Each opening runs on its own served node. A duplicate (`fresh = false`)
first opens the goal and then revokes the request's relationships, which is
the state the model starts a duplicate from. -/
@[conformance_family]
def goalBackedRuntime : Family RuntimeGoalOpeningInput GoalAutomation.RuntimeGoalPublication where
  inputs :=
    [ ⟨"created", "agent", "agent", "agent", "session", "session", "agent-doc", "agent-doc", true, true⟩
    , ⟨"aborted", "agent", "agent", "agent", "session", "session", "agent-doc", "agent-doc", true, false⟩
    , ⟨"foreign-actor", "other", "agent", "agent", "session", "session", "agent-doc", "agent-doc", true, true⟩
    , ⟨"wrong-session", "agent", "agent", "agent", "other", "session", "agent-doc", "agent-doc", true, true⟩
    , ⟨"wrong-agent", "agent", "agent", "agent", "session", "session", "other", "agent-doc", true, true⟩
    , ⟨"duplicate-after-revocation", "agent", "agent", "agent", "session", "session", "agent-doc",
        "agent-doc", false, true⟩ ]
  model := fun i =>
    let before : GoalAutomation.RuntimeGoalPublication :=
      if i.fresh then ⟨false, false, false, false⟩ else ⟨true, true, true, false⟩
    GoalAutomation.commitRuntimeGoalOpening before i.actor i.signer i.selected
      i.targetSession i.observedSession i.targetAgent i.observedAgent i.fresh i.commit

def goalSubmissionCases : List GoalSubmissionCase :=
  [ { name := "stage_goal_not_visible", state := ⟨false, false, false, false⟩,
      action := .stageGoal }
  , { name := "request_first_not_visible", state := ⟨false, false, false, false⟩,
      action := .stageRequest }
  , { name := "goal_after_request", state := ⟨false, false, false, true⟩,
      action := .stageGoal }
  , { name := "commit_both_atomically", state := ⟨false, false, true, true⟩,
      action := .commit }
  , { name := "failure_between_writes_discards_staging", state := ⟨false, false, true, false⟩,
      action := .crash }
  , { name := "committed_retry_is_idempotent", state := ⟨true, true, false, false⟩,
      action := .commit }
  ]

deriving instance Lean.ToJson for GoalAutomation.SubmissionState,
  GoalAutomation.SubmissionAction, GoalSubmissionCase

@[conformance_family]
def goalSubmission : Family GoalSubmissionCase GoalAutomation.SubmissionState where
  inputs := goalSubmissionCases
  model := fun w => GoalAutomation.submissionStep w.state w.action

structure GoalSubmissionPublication where
  durableGoal : Bool
  runnableRequest : Bool
  deriving Lean.ToJson

/-- A served node exposes only committed submissions: staged rows live inside
the publishing transaction and a crash discards them, so the publishing
scenario can build only the committed case. -/
def goalSubmissionPublishedCases : List GoalSubmissionCase :=
  goalSubmissionCases.filter fun w => w.name ∈ ["commit_both_atomically"]

@[conformance_family]
def goalSubmissionPublished : Family GoalSubmissionCase GoalSubmissionPublication where
  inputs := goalSubmissionPublishedCases
  model := fun w =>
    let post := GoalAutomation.submissionStep w.state w.action
    { durableGoal := post.durableGoal, runnableRequest := post.runnableRequest }

structure GoalContinuationMaterializationCase where
  name : String
  phase : GoalAutomation.ContinuationPhase
  action : GoalAutomation.ContinuationAction

def goalContinuationMaterializationCases : List GoalContinuationMaterializationCase :=
  [ { name := "eligible_claim", phase := .unclaimed, action := .claim true }
  , { name := "ineligible_claim_rejected", phase := .unclaimed, action := .claim false }
  , { name := "crash_preserves_claim", phase := .claimed, action := .crash }
  , { name := "restart_materializes_claimed_child", phase := .claimed, action := .reconcile }
  , { name := "restart_does_not_duplicate_child", phase := .childPresent, action := .reconcile }
  ]

inductive ContinuationActionWire where
  | claimEligible
  | claimIneligible
  | materialize
  | reconcile
  | crash
  deriving Lean.ToJson

def ContinuationActionWire.ofModel : GoalAutomation.ContinuationAction → ContinuationActionWire
  | .claim true => .claimEligible
  | .claim false => .claimIneligible
  | .materialize => .materialize
  | .reconcile => .reconcile
  | .crash => .crash

def ContinuationActionWire.toModel : ContinuationActionWire → GoalAutomation.ContinuationAction
  | .claimEligible => .claim true
  | .claimIneligible => .claim false
  | .materialize => .materialize
  | .reconcile => .reconcile
  | .crash => .crash

deriving instance Lean.ToJson for GoalAutomation.ContinuationPhase

structure ContinuationMaterializationInput where
  name : String
  phase : GoalAutomation.ContinuationPhase
  action : ContinuationActionWire
  deriving Lean.ToJson

structure ContinuationMaterializationOutput where
  phase : GoalAutomation.ContinuationPhase
  deriving Lean.ToJson

@[conformance_family]
def goalContinuationMaterialization :
    Family ContinuationMaterializationInput ContinuationMaterializationOutput where
  inputs := goalContinuationMaterializationCases.map fun w =>
    { name := w.name, phase := w.phase, action := ContinuationActionWire.ofModel w.action }
  model := fun i => { phase := GoalAutomation.continuationStep i.phase i.action.toModel }

section ReadinessGate

open GoalAutomation.ReadinessGate

structure GoalReadinessGateCase where
  name : String
  observation : Observation
  settled : Bool
  cause : Cause
  input : Input

private def activeInput (terminal : Goals.RequestTerminal) (retries : Nat := 0)
    (budget : Bool := false) (child : Bool := false) (idle : Bool := true)
    (maxRetries : Nat := Goals.maxInfrastructureRetries) : Input :=
  { status := .active, terminal, sessionIdle := idle, childExists := child,
    budgetReached := budget, hasActivity := true, requestIsWrapup := false, retries,
    wrapupRequested := false, wrapupCompleted := false, maxRetries }

private def pendingWrapupInput (terminal : Goals.RequestTerminal) (retries : Nat) : Input :=
  { status := .budgetLimited, terminal, sessionIdle := true, childExists := false,
    budgetReached := true, hasActivity := true, requestIsWrapup := true, retries,
    wrapupRequested := true, wrapupCompleted := false }

def goalReadinessGateCases : List GoalReadinessGateCase :=
  [ { name := "missing_readiness_defers_continuation", observation := .unknown,
      settled := true, cause := .attempt, input := activeInput .completed }
  , { name := "backend_recovery_defers_even_when_settled", observation := .backendRecovering,
      settled := true, cause := .attempt, input := activeInput .completed }
  , { name := "unsettled_invalid_agent_defers", observation := .unavailable,
      settled := false, cause := .attempt, input := activeInput .completed }
  , { name := "settled_invalid_agent_stops_active_goal", observation := .unavailable,
      settled := true, cause := .attempt, input := activeInput .completed }
  , { name := "settled_unassigned_agent_stops_active_goal", observation := .unassigned,
      settled := true, cause := .attempt, input := activeInput .completed }
  , { name := "ready_completed_attempt_continues_and_clears_retries", observation := .ready true,
      settled := true, cause := .attempt, input := activeInput .completed 1 }
  , { name := "readiness_rejection_while_waiting_is_uncharged", observation := .unknown,
      settled := false, cause := .agentUnavailable, input := activeInput .failed 1 }
  , { name := "readiness_rejection_after_recovery_continues_uncharged", observation := .ready true,
      settled := true, cause := .agentUnavailable, input := activeInput .failed 1 }
  , { name := "readiness_rejection_at_exhausted_budget_does_not_pause", observation := .ready true,
      settled := true, cause := .agentUnavailable, input := activeInput .failed 2 }
  , { name := "readiness_rejection_over_token_budget_requests_wrapup", observation := .ready true,
      settled := true, cause := .agentUnavailable, input := activeInput .failed 1 true }
  , { name := "executed_failure_is_charged", observation := .ready true,
      settled := true, cause := .attempt, input := activeInput .failed 0 }
  , { name := "executed_failure_exhausts_bound", observation := .ready true,
      settled := true, cause := .attempt, input := activeInput .dead 2 }
  , { name := "executed_failure_while_waiting_defers_charge", observation := .backendRecovering,
      settled := true, cause := .attempt, input := activeInput .failed 0 }
  , { name := "exhausted_failure_pauses_without_readiness", observation := .unavailable,
      settled := true, cause := .attempt, input := activeInput .failed 2 }
  , { name := "readiness_rejected_wrapup_reissues_uncharged", observation := .ready true,
      settled := true, cause := .agentUnavailable, input := pendingWrapupInput .failed 1 }
  , { name := "settled_unavailable_abandons_pending_wrapup", observation := .unavailable,
      settled := true, cause := .attempt, input := pendingWrapupInput .failed 0 }
  , { name := "existing_child_is_never_duplicated", observation := .ready true,
      settled := true, cause := .agentUnavailable,
      input := activeInput .failed 0 (child := true) }
  , { name := "stale_readiness_defers_rejection_reissue", observation := .ready false,
      settled := true, cause := .agentUnavailable, input := activeInput .failed 1 }
  , { name := "stale_readiness_still_decides_an_executed_attempt", observation := .ready false,
      settled := true, cause := .attempt, input := activeInput .failed 0 }
  , { name := "completed_goal_control_failure_is_charged", observation := .ready true,
      settled := true, cause := .goalControlUnavailable, input := activeInput .completed 0 }
  , { name := "repeated_goal_control_failure_uses_second_retry", observation := .ready true,
      settled := true, cause := .goalControlUnavailable, input := activeInput .completed 1 }
  , { name := "generated_text_cannot_reset_goal_control_failures", observation := .ready true,
      settled := true, cause := .goalControlUnavailable, input := activeInput .completed 2 }
  , { name := "exhausted_goal_control_failure_pauses_even_without_readiness", observation := .unknown,
      settled := true, cause := .goalControlUnavailable, input := activeInput .completed 2 }
  , { name := "goal_control_failure_does_not_duplicate_a_child", observation := .ready true,
      settled := true, cause := .goalControlUnavailable, input := activeInput .completed 0 (child := true) }
  , { name := "busy_session_never_continues", observation := .unavailable,
      settled := true, cause := .attempt, input := activeInput .completed (idle := false) }
  , { name := "configured_retry_limit_charges_past_default", observation := .ready true,
      settled := true, cause := .attempt, input := activeInput .failed 2 (maxRetries := 3) }
  , { name := "zero_retry_limit_pauses_goal_control_failure", observation := .ready true,
      settled := true, cause := .goalControlUnavailable,
      input := activeInput .completed 0 (maxRetries := 0) }
  ]

inductive GoalObservationWire where
  | ready
  | backendRecovering
  | unavailable
  | unassigned
  | unknown
  deriving Lean.ToJson

def GoalObservationWire.ofModel : Observation → GoalObservationWire
  | .ready _ => .ready
  | .backendRecovering => .backendRecovering
  | .unavailable => .unavailable
  | .unassigned => .unassigned
  | .unknown => .unknown

def GoalObservationWire.toModel (newerThanTerminal : Bool) : GoalObservationWire → Observation
  | .ready => .ready newerThanTerminal
  | .backendRecovering => .backendRecovering
  | .unavailable => .unavailable
  | .unassigned => .unassigned
  | .unknown => .unknown

inductive GatedWire where
  | none
  | continue
  | retry
  | pause
  | wrapup
  | abandonWrapup
  | awaitReadiness
  | agentUnavailable
  deriving Lean.ToJson

def GatedWire.ofModel : Gated → GatedWire
  | .decided .none => .none
  | .decided .continue => .continue
  | .decided .retry => .retry
  | .decided .pause => .pause
  | .decided .wrapup => .wrapup
  | .decided .abandonWrapup => .abandonWrapup
  | .awaitReadiness => .awaitReadiness
  | .agentUnavailable => .agentUnavailable

deriving instance Lean.ToJson for Readiness, Cause

structure ReadinessGateInput where
  name : String
  observation : GoalObservationWire
  newerThanTerminal : Bool
  settled : Bool
  cause : Cause
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
  maxInfrastructureRetries : Nat
  deriving Lean.ToJson

def ReadinessGateInput.ofCase (w : GoalReadinessGateCase) : ReadinessGateInput :=
  { name := w.name, observation := GoalObservationWire.ofModel w.observation
  , newerThanTerminal := w.observation.newerThanTerminal, settled := w.settled, cause := w.cause
  , status := w.input.status, terminal := w.input.terminal, sessionIdle := w.input.sessionIdle
  , childExists := w.input.childExists, budgetReached := w.input.budgetReached
  , hasActivity := w.input.hasActivity, requestIsWrapup := w.input.requestIsWrapup
  , infrastructureRetries := w.input.retries, wrapupRequested := w.input.wrapupRequested
  , wrapupCompleted := w.input.wrapupCompleted
  , maxInfrastructureRetries := w.input.maxRetries }

def ReadinessGateInput.toObservation (i : ReadinessGateInput) : Observation :=
  i.observation.toModel i.newerThanTerminal

def ReadinessGateInput.toInput (i : ReadinessGateInput) : Input :=
  { status := i.status, terminal := i.terminal, sessionIdle := i.sessionIdle
  , childExists := i.childExists, budgetReached := i.budgetReached, hasActivity := i.hasActivity
  , requestIsWrapup := i.requestIsWrapup, retries := i.infrastructureRetries
  , wrapupRequested := i.wrapupRequested, wrapupCompleted := i.wrapupCompleted
  , maxRetries := i.maxInfrastructureRetries }

structure ReadinessGateOutput where
  readiness : Readiness
  gate : GatedWire
  retries : Nat
  deriving Lean.ToJson

@[conformance_family]
def goalReadinessGate : Family ReadinessGateInput ReadinessGateOutput where
  inputs := goalReadinessGateCases.map ReadinessGateInput.ofCase
  model := fun i =>
    let gated := GoalAutomation.ReadinessGate.gate i.toObservation i.settled i.cause i.toInput
    { readiness := observe i.toObservation i.settled
    , gate := GatedWire.ofModel gated
    , retries := nextRetries i.cause i.toInput gated }

structure ReadinessGateGoalWrite where
  gate : GatedWire
  retries : Nat
  deriving Lean.ToJson

/-- The goal source reads readiness from the node readiness document and the
reconcile phase, in order, on one active goal after one completed attempt: only
the attempt cases whose observation that document and phase can publish are
built, and the goal paused by the last one ends the sequence. -/
def goalReadinessGateServedCases : List GoalReadinessGateCase :=
  goalReadinessGateCases.filter fun w => w.name ∈
    [ "missing_readiness_defers_continuation", "backend_recovery_defers_even_when_settled"
    , "unsettled_invalid_agent_defers", "settled_invalid_agent_stops_active_goal" ]

@[conformance_family]
def goalReadinessGateServed : Family ReadinessGateInput ReadinessGateGoalWrite where
  inputs := goalReadinessGateServedCases.map ReadinessGateInput.ofCase
  model := fun i =>
    let gated := GoalAutomation.ReadinessGate.gate i.toObservation i.settled i.cause i.toInput
    { gate := GatedWire.ofModel gated, retries := nextRetries i.cause i.toInput gated }

end ReadinessGate

end Conformance.Contracts
