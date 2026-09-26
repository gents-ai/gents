import Proofs.TaskHooks
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.TaskHooksContracts
open TaskHooks Conformance.Contracts

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def phaseString : HookPhase → String
  | .before => "before"
  | .afterSuccess => "after_success"
  | .afterFailure => "after_failure"
  | HookPhase.finally => "finally"

private def hookJson (h : TaskHook) : String :=
  "{\"hook_id\":" ++ jsonString h.hookId ++
    ",\"phase\":" ++ jsonString (phaseString h.phase) ++
    ",\"command\":" ++ jsonStringArray h.command ++
    ",\"timeout_secs\":" ++ (h.timeoutSecs.map toString).getD "null" ++
    ",\"effective_timeout_secs\":" ++ toString h.effectiveTimeout ++ "}"

private def commandResultJson : CommandResult → String
  | .exited code => "{\"kind\":\"exited\",\"code\":" ++ toString code ++ "}"
  | .launchFailed => "{\"kind\":\"launch_failed\"}"
  | .timedOut => "{\"kind\":\"timed_out\"}"
  | .interrupted => "{\"kind\":\"interrupted\"}"

private def attemptJson (a : HookAttempt) : String :=
  "{\"hook_id\":" ++ jsonString a.hookId ++
    ",\"result\":" ++ commandResultJson a.result ++
    ",\"succeeded\":" ++ boolJson a.result.succeeded ++ "}"

private def primaryErrorJson : PrimaryError → String
  | .hook id => "{\"kind\":\"hook\",\"hook_id\":" ++ jsonString id ++ "}"
  | .agent => "{\"kind\":\"agent\"}"

private def outcomeJson : TaskOutcome → String
  | .success => "{\"kind\":\"success\"}"
  | .failure error => "{\"kind\":\"failure\",\"error\":" ++ primaryErrorJson error ++ "}"
  | .interrupted => "{\"kind\":\"interrupted\"}"

private def agentResultString : AgentResult → String
  | .success => "success"
  | .failure => "failure"
  | .cancelled => "cancelled"
  | .interrupted => "interrupted"

structure AdmissionCase where
  name : String
  hooks : List TaskHook
  expectedAdmitted : Bool
  deriving Repr

private def before (hookId : String) (timeoutSecs : Option Int := none) : TaskHook :=
  { hookId := hookId, phase := .before, command := ["bin/" ++ hookId],
    timeoutSecs := timeoutSecs }

private def afterSuccess (hookId : String) : TaskHook :=
  { hookId := hookId, phase := .afterSuccess, command := ["bin/" ++ hookId] }

private def afterFailure (hookId : String) : TaskHook :=
  { hookId := hookId, phase := .afterFailure, command := ["bin/" ++ hookId] }

private def cleanup (hookId : String) : TaskHook :=
  { hookId := hookId, phase := HookPhase.finally, command := ["bin/" ++ hookId] }

def admissionCases : List AdmissionCase :=
  [ { name := "no_hooks_admitted", hooks := [], expectedAdmitted := true }
  , { name := "default_timeout_admitted"
    , hooks := [before "prepare"], expectedAdmitted := true }
  , { name := "explicit_positive_timeout_admitted"
    , hooks := [before "prepare" (some 30)], expectedAdmitted := true }
  , { name := "every_phase_admitted"
    , hooks := [before "prepare", afterSuccess "verify", afterFailure "report",
                cleanup "sweep"]
    , expectedAdmitted := true }
  , { name := "empty_command_rejected"
    , hooks := [{ before "prepare" with command := [] }], expectedAdmitted := false }
  , { name := "zero_timeout_rejected"
    , hooks := [before "prepare" (some 0)], expectedAdmitted := false }
  , { name := "negative_timeout_rejected"
    , hooks := [before "prepare" (some (-1))], expectedAdmitted := false }
  , { name := "duplicate_id_same_phase_rejected"
    , hooks := [before "prepare", before "prepare"], expectedAdmitted := false }
  , { name := "duplicate_id_across_phases_rejected"
    , hooks := [before "prepare", { cleanup "sweep" with hookId := "prepare" }]
    , expectedAdmitted := false }
  , { name := "one_invalid_hook_rejects_whole_task"
    , hooks := [before "prepare", { cleanup "sweep" with timeoutSecs := some (-5) }]
    , expectedAdmitted := false } ]

theorem admissionCases_replay : ∀ c ∈ admissionCases,
    (admitHooks c.hooks).isSome = c.expectedAdmitted := by decide

private def admissionCaseJson (c : AdmissionCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"hooks\":" ++ jsonArray (c.hooks.map hookJson) ++
    ",\"expected_admitted\":" ++ boolJson c.expectedAdmitted ++ "}"

def admissionCasesJson : String := jsonArray (admissionCases.map admissionCaseJson)

structure ScriptedResult where
  hookId : String
  result : CommandResult
  deriving DecidableEq, Repr

/-- An unscripted hook is observed to have exited zero; `HookExec` is total. -/
private def scriptedExec (script : List ScriptedResult) : HookExec := fun h =>
  match script.find? (fun s => s.hookId == h.hookId) with
  | some s => s.result
  | none => .exited 0

structure RunCase where
  name : String
  hooks : List TaskHook
  script : List ScriptedResult
  agent : AgentResult
  expectedOutcome : TaskOutcome
  expectedFinalOutcome : TaskOutcome
  expectedAgentRan : Bool
  deriving Repr

private def runOf (c : RunCase) : RunResult :=
  runTask c.hooks (scriptedExec c.script) c.agent

private def failed (hookId : String) : ScriptedResult := ⟨hookId, .exited 1⟩
private def interruptedAt (hookId : String) : ScriptedResult := ⟨hookId, .interrupted⟩

private def fullTask : List TaskHook :=
  [before "prepare", afterSuccess "verify", afterFailure "report", cleanup "sweep"]

def runCases : List RunCase :=
  [ { name := "agent_success_runs_after_success_then_cleanup"
    , hooks := fullTask, script := [], agent := .success
    , expectedOutcome := .success, expectedFinalOutcome := .success
    , expectedAgentRan := true }
  , { name := "after_success_failure_blocks_successful_completion"
    , hooks := fullTask, script := [failed "verify"], agent := .success
    , expectedOutcome := .failure (.hook "verify")
    , expectedFinalOutcome := .failure (.hook "verify")
    , expectedAgentRan := true }
  , { name := "after_success_timeout_blocks_successful_completion"
    , hooks := fullTask, script := [⟨"verify", .timedOut⟩], agent := .success
    , expectedOutcome := .failure (.hook "verify")
    , expectedFinalOutcome := .failure (.hook "verify")
    , expectedAgentRan := true }
  , { name := "after_success_launch_failure_blocks_successful_completion"
    , hooks := fullTask, script := [⟨"verify", .launchFailed⟩], agent := .success
    , expectedOutcome := .failure (.hook "verify")
    , expectedFinalOutcome := .failure (.hook "verify")
    , expectedAgentRan := true }
  , { name := "before_failure_prevents_agent_and_selects_after_failure"
    , hooks := fullTask, script := [failed "prepare"], agent := .success
    , expectedOutcome := .failure (.hook "prepare")
    , expectedFinalOutcome := .failure (.hook "prepare")
    , expectedAgentRan := false }
  , { name := "before_stops_at_first_failure"
    , hooks := [before "first", before "second", cleanup "sweep"]
    , script := [failed "first"], agent := .success
    , expectedOutcome := .failure (.hook "first")
    , expectedFinalOutcome := .failure (.hook "first")
    , expectedAgentRan := false }
  , { name := "agent_failure_stays_primary_over_after_failure_hook_error"
    , hooks := fullTask, script := [failed "report"], agent := .failure
    , expectedOutcome := .failure .agent
    , expectedFinalOutcome := .failure .agent
    , expectedAgentRan := true }
  , { name := "cleanup_failure_turns_success_into_failure"
    , hooks := fullTask, script := [failed "sweep"], agent := .success
    , expectedOutcome := .success
    , expectedFinalOutcome := .failure (.hook "sweep")
    , expectedAgentRan := true }
  , { name := "cleanup_failure_does_not_erase_agent_failure"
    , hooks := fullTask, script := [failed "sweep"], agent := .failure
    , expectedOutcome := .failure .agent
    , expectedFinalOutcome := .failure .agent
    , expectedAgentRan := true }
  , { name := "every_cleanup_hook_attempted_after_a_cleanup_error"
    , hooks := [cleanup "first", cleanup "second"]
    , script := [failed "first"], agent := .success
    , expectedOutcome := .success
    , expectedFinalOutcome := .failure (.hook "first")
    , expectedAgentRan := true }
  , { name := "cleanup_runs_after_a_failing_before_hook"
    , hooks := [before "prepare", cleanup "sweep"]
    , script := [failed "prepare"], agent := .success
    , expectedOutcome := .failure (.hook "prepare")
    , expectedFinalOutcome := .failure (.hook "prepare")
    , expectedAgentRan := false }
  , { name := "cancelled_agent_skips_outcome_phases_and_still_cleans_up"
    , hooks := fullTask, script := [], agent := .cancelled
    , expectedOutcome := .interrupted, expectedFinalOutcome := .interrupted
    , expectedAgentRan := true }
  , { name := "interrupted_agent_skips_outcome_phases_and_still_cleans_up"
    , hooks := fullTask, script := [], agent := .interrupted
    , expectedOutcome := .interrupted, expectedFinalOutcome := .interrupted
    , expectedAgentRan := true }
  , { name := "interrupted_before_hook_reports_interruption_not_failure"
    , hooks := fullTask, script := [interruptedAt "prepare"], agent := .success
    , expectedOutcome := .interrupted, expectedFinalOutcome := .interrupted
    , expectedAgentRan := false }
  , { name := "interrupted_cleanup_after_success_reports_interruption"
    , hooks := [cleanup "sweep"], script := [interruptedAt "sweep"], agent := .success
    , expectedOutcome := .success, expectedFinalOutcome := .interrupted
    , expectedAgentRan := true }
  , { name := "first_cleanup_failure_survives_a_later_cleanup_interruption"
    , hooks := [cleanup "first", cleanup "second"]
    , script := [failed "first", interruptedAt "second"], agent := .success
    , expectedOutcome := .success
    , expectedFinalOutcome := .failure (.hook "first")
    , expectedAgentRan := true }
  , { name := "task_without_hooks_reports_agent_outcome"
    , hooks := [], script := [], agent := .failure
    , expectedOutcome := .failure .agent
    , expectedFinalOutcome := .failure .agent
    , expectedAgentRan := true } ]

theorem runCases_admitted : ∀ c ∈ runCases, admitHooks c.hooks = some c.hooks := by decide

theorem runCases_replay : ∀ c ∈ runCases,
    (runOf c).outcome = c.expectedOutcome ∧
      (runOf c).finalOutcome = c.expectedFinalOutcome ∧
      (runOf c).agentResult.isSome = c.expectedAgentRan := by decide

private def runCaseJson (c : RunCase) : String :=
  let result := runOf c
  "{\"name\":" ++ jsonString c.name ++
    ",\"hooks\":" ++ jsonArray (c.hooks.map hookJson) ++
    ",\"script\":" ++ jsonArray (c.script.map (fun s =>
      "{\"hook_id\":" ++ jsonString s.hookId ++
        ",\"result\":" ++ commandResultJson s.result ++ "}")) ++
    ",\"agent\":" ++ jsonString (agentResultString c.agent) ++
    ",\"expected_agent_ran\":" ++ boolJson c.expectedAgentRan ++
    ",\"before_attempted\":" ++ jsonArray (result.beforeAttempted.map attemptJson) ++
    ",\"after_success_attempted\":" ++
      jsonArray (result.afterSuccessAttempted.map attemptJson) ++
    ",\"after_failure_attempted\":" ++
      jsonArray (result.afterFailureAttempted.map attemptJson) ++
    ",\"finally_attempted\":" ++ jsonArray (result.finallyAttempted.map attemptJson) ++
    ",\"cleanup_errors\":" ++ jsonStringArray result.cleanupErrors ++
    ",\"expected_outcome\":" ++ outcomeJson c.expectedOutcome ++
    ",\"expected_final_outcome\":" ++ outcomeJson c.expectedFinalOutcome ++
    ",\"expected_request_state\":" ++
      jsonString c.expectedFinalOutcome.toRequestState.toDefraDB ++ "}"

def runCasesJson : String := jsonArray (runCases.map runCaseJson)

structure RecoveryCase where
  name : String
  started : Bool
  hooks : List TaskHook
  observed : List HookAttempt
  script : List ScriptedResult
  expectedRemaining : List String
  deriving Repr

private def recoveryOf (c : RecoveryCase) : RequestState × List HookAttempt :=
  recoverInterrupted c.started c.hooks c.observed (scriptedExec c.script)

def recoveryCases : List RecoveryCase :=
  [ { name := "before_start_runs_no_cleanup"
    , started := false, hooks := [cleanup "sweep"], observed := [], script := []
    , expectedRemaining := [] }
  , { name := "started_runs_every_unobserved_cleanup_hook"
    , started := true, hooks := fullTask, observed := [], script := []
    , expectedRemaining := ["sweep"] }
  , { name := "observed_cleanup_is_not_repeated"
    , started := true, hooks := [cleanup "first", cleanup "second"]
    , observed := [⟨"first", .exited 0⟩], script := []
    , expectedRemaining := ["second"] }
  , { name := "unknown_outcome_cleanup_is_not_replayed"
    , started := true, hooks := [cleanup "first", cleanup "second"]
    , observed := [⟨"first", .interrupted⟩], script := []
    , expectedRemaining := ["second"] }
  , { name := "recovery_selects_only_cleanup_hooks"
    , started := true, hooks := [before "prepare", afterSuccess "verify",
                                 afterFailure "report", cleanup "sweep"]
    , observed := [⟨"prepare", .exited 0⟩], script := []
    , expectedRemaining := ["sweep"] }
  , { name := "remaining_cleanup_errors_do_not_stop_recovery"
    , started := true, hooks := [cleanup "first", cleanup "second"]
    , observed := [], script := [failed "first"]
    , expectedRemaining := ["first", "second"] } ]

theorem recoveryCases_replay : ∀ c ∈ recoveryCases,
    (recoveryOf c).1 = RequestState.interrupted ∧
      (recoveryOf c).2.map HookAttempt.hookId = c.expectedRemaining := by decide

private def recoveryCaseJson (c : RecoveryCase) : String :=
  let result := recoveryOf c
  "{\"name\":" ++ jsonString c.name ++
    ",\"started\":" ++ boolJson c.started ++
    ",\"hooks\":" ++ jsonArray (c.hooks.map hookJson) ++
    ",\"observed\":" ++ jsonArray (c.observed.map attemptJson) ++
    ",\"script\":" ++ jsonArray (c.script.map (fun s =>
      "{\"hook_id\":" ++ jsonString s.hookId ++
        ",\"result\":" ++ commandResultJson s.result ++ "}")) ++
    ",\"expected_remaining\":" ++ jsonStringArray c.expectedRemaining ++
    ",\"attempted\":" ++ jsonArray (result.2.map attemptJson) ++
    ",\"expected_request_state\":" ++ jsonString result.1.toDefraDB ++ "}"

def recoveryCasesJson : String := jsonArray (recoveryCases.map recoveryCaseJson)

end Conformance.TaskHooksContracts
