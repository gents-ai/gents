import Proofs.Goals
import Proofs.GoalAutomation
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

structure GoalCreateCase where
  name : String
  request : GoalAutomation.CreateRequest
  existing : Option GoalAutomation.Fingerprint

def createRequest (owner session objective : String) (budget : Option Int := none)
    (goalTools : Bool := true) (goalCreate : Bool := true) :
    GoalAutomation.CreateRequest :=
  { caller := owner, currentSession := session
  , requestedOwner := owner, requestedSession := session
  , objective := objective, objectiveNonempty := !objective.isEmpty
  , tokenBudget := budget, goalTools := goalTools, goalCreate := goalCreate }

def goalCreateCases : List GoalCreateCase :=
  let base := createRequest "did:a" "session-a" "ship feature"
  let mismatch : GoalAutomation.Fingerprint :=
    { owner := "did:a", session := "session-a"
    , objective := "different", tokenBudget := none }
  let spaced := createRequest "did:a" "session-a" "  ship feature  "
  let canonical : GoalAutomation.Fingerprint :=
    { owner := "did:a", session := "session-a"
    , objective := "ship feature", tokenBudget := none }
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
  ]

def createDispositionName : GoalAutomation.CreateDisposition → String
  | .denied => "denied"
  | .invalid => "invalid"
  | .fresh => "fresh"
  | .idempotent => "idempotent"
  | .conflict => "conflict"

def goalCreateCaseJson (w : GoalCreateCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"caller\":" ++ jsonString w.request.caller ++ ","
    ++ "\"current_session\":" ++ jsonString w.request.currentSession ++ ","
    ++ "\"requested_owner\":" ++ jsonString w.request.requestedOwner ++ ","
    ++ "\"requested_session\":" ++ jsonString w.request.requestedSession ++ ","
    ++ "\"objective\":" ++ jsonString w.request.objective ++ ","
    ++ "\"objective_nonempty\":" ++ boolString w.request.objectiveNonempty ++ ","
    ++ "\"token_budget\":" ++ (match w.request.tokenBudget with
      | none => "null,"
      | some value => toString value ++ ",")
    ++ "\"goal_tools\":" ++ boolString w.request.goalTools ++ ","
    ++ "\"goal_create\":" ++ boolString w.request.goalCreate ++ ","
    ++ "\"existing\":" ++ boolString w.existing.isSome ++ ","
    ++ "\"existing_matches\":" ++ boolString
      (w.existing == some (GoalAutomation.createFingerprint w.request)) ++ ","
    ++ "\"expected\":" ++ jsonString
      (createDispositionName (GoalAutomation.decideCreate w.request w.existing))
    ++ "}"

def goalCreateCasesJson : String := jsonArray (goalCreateCases.map goalCreateCaseJson)

structure TaskGoalPublicationCase where
  name : String
  agentDid : String
  taskId : String
  fireKey : String
  declaration : GoalAutomation.TaskGoalDeclaration

def taskGoalPublicationCases : List TaskGoalPublicationCase :=
  let agentDid := "did:key:z-feature-owner"
  let taskId := "feature-implementation"
  let fireKey := "schedule:2026-09-02T12:00:00Z"
  [ { name := "absent_goal_uses_ordinary_publication", agentDid, taskId, fireKey
      declaration := ⟨none, none⟩ }
  , { name := "objective_uses_atomic_goal_publication", agentDid, taskId, fireKey
      declaration := ⟨some "implement feature", none⟩ }
  , { name := "positive_budget_uses_atomic_goal_publication", agentDid, taskId, fireKey
      declaration := ⟨some "implement feature", some 1000⟩ }
  , { name := "maximum_budget_uses_atomic_goal_publication", agentDid, taskId, fireKey
      declaration := ⟨some "implement feature", some GoalAutomation.maxTokenBudget⟩ }
  , { name := "same_task_fire_is_scoped_to_other_principal",
      agentDid := "did:key:z-other-feature-owner", taskId, fireKey
      declaration := ⟨some "implement feature", none⟩ }
  , { name := "blank_objective_cannot_publish", agentDid, taskId, fireKey
      declaration := ⟨some "", none⟩ }
  , { name := "whitespace_objective_cannot_publish", agentDid, taskId, fireKey
      declaration := ⟨some "   ", none⟩ }
  , { name := "budget_without_objective_cannot_publish", agentDid, taskId, fireKey
      declaration := ⟨none, some 1000⟩ }
  , { name := "zero_budget_cannot_publish", agentDid, taskId, fireKey
      declaration := ⟨some "implement feature", some 0⟩ }
  , { name := "negative_budget_cannot_publish", agentDid, taskId, fireKey
      declaration := ⟨some "implement feature", some (-1)⟩ }
  , { name := "overflow_budget_cannot_publish", agentDid, taskId, fireKey
      declaration := ⟨some "implement feature",
        some (GoalAutomation.maxTokenBudget + 1)⟩ }
  ]

def taskPublicationModeName : GoalAutomation.TaskPublicationMode → String
  | .invalid => "invalid"
  | .ordinary => "ordinary"
  | .atomicGoalBacked => "atomic_goal_backed"

def goalOptionalStringJson : Option String → String
  | none => "null"
  | some value => jsonString value

def goalOptionalIntJson : Option Int → String
  | none => "null"
  | some value => toString value

def taskGoalPublicationCaseJson (w : TaskGoalPublicationCase) : String :=
  let publication := GoalAutomation.decideTaskPublication
    w.declaration w.agentDid w.taskId w.fireKey
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"agent_did\":" ++ jsonString w.agentDid ++ ","
    ++ "\"task_id\":" ++ jsonString w.taskId ++ ","
    ++ "\"fire_key\":" ++ jsonString w.fireKey ++ ","
    ++ "\"goal_objective\":" ++ goalOptionalStringJson w.declaration.goalObjective ++ ","
    ++ "\"goal_token_budget\":" ++ goalOptionalIntJson w.declaration.goalTokenBudget ++ ","
    ++ "\"declaration_valid\":" ++ boolString
      (GoalAutomation.validTaskGoalDeclaration w.declaration) ++ ","
    ++ "\"expected_mode\":" ++ jsonString (taskPublicationModeName publication.mode) ++ ","
    ++ "\"expected_published\":" ++ boolString publication.published ++ ","
    ++ "\"expected_runnable_request\":" ++ boolString publication.runnableRequest ++ ","
    ++ "\"expected_durable_goal\":" ++ boolString publication.durableGoal ++ ","
    ++ "\"expected_session_id\":" ++ goalOptionalStringJson publication.sessionId ++ ","
    ++ "\"expected_request_id\":" ++ goalOptionalStringJson publication.requestId ++ ","
    ++ "\"expected_retry_key\":" ++ goalOptionalStringJson publication.retryKey
    ++ "}"

def taskGoalPublicationCasesJson : String :=
  jsonArray (taskGoalPublicationCases.map taskGoalPublicationCaseJson)

structure TaskGoalRecoveryCase where
  name : String
  agentDid : String
  behaviorId : String
  taskId : String
  fireKey : String
  request : Option GoalAutomation.TaskGoalRequestBinding
  durableGoalPresent : Bool
  creationClaimPresent : Bool

def taskGoalRecoveryCases : List TaskGoalRecoveryCase :=
  let agentDid := "did:key:z-feature-owner"
  let behaviorId := "feature-engineer"
  let taskId := "feature-implementation"
  let fireKey := "schedule:2026-09-02T12:00:00Z"
  let expected := GoalAutomation.expectedTaskGoalRequestBinding
    agentDid behaviorId taskId fireKey
  [ { name := "request_absent_has_nothing_to_checkpoint", agentDid, behaviorId, taskId, fireKey
      request := none, durableGoalPresent := false, creationClaimPresent := false }
  , { name := "matching_request_with_metadata_recovers", agentDid, behaviorId, taskId, fireKey
      request := some expected, durableGoalPresent := true, creationClaimPresent := true }
  , { name := "matching_request_without_goal_recovers", agentDid, behaviorId, taskId, fireKey
      request := some expected, durableGoalPresent := false, creationClaimPresent := true }
  , { name := "matching_request_without_claim_recovers", agentDid, behaviorId, taskId, fireKey
      request := some expected, durableGoalPresent := true, creationClaimPresent := false }
  , { name := "matching_request_without_goal_or_claim_recovers",
      agentDid, behaviorId, taskId, fireKey
      request := some expected, durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_principal_conflicts",
      agentDid, behaviorId, taskId, fireKey
      request := some { expected with agentDid := "did:key:z-other-owner" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_behavior_conflicts", agentDid, behaviorId, taskId, fireKey
      request := some { expected with behaviorId := "other-behavior" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_session_conflicts", agentDid, behaviorId, taskId, fireKey
      request := some { expected with sessionId := "other-session" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_request_id_conflicts", agentDid, behaviorId, taskId, fireKey
      request := some { expected with requestId := "other-request" }
      durableGoalPresent := false, creationClaimPresent := false }
  , { name := "mismatched_retry_key_conflicts", agentDid, behaviorId, taskId, fireKey
      request := some { expected with retryKey := "other-retry" }
      durableGoalPresent := false, creationClaimPresent := false }
  ]

def taskFireRecoveryDispositionName : GoalAutomation.TaskFireRecoveryDisposition → String
  | .absent => "absent"
  | .recovered => "recovered"
  | .conflict => "conflict"

def taskGoalRecoveryCaseJson (w : TaskGoalRecoveryCase) : String :=
  let expected := GoalAutomation.expectedTaskGoalRequestBinding
    w.agentDid w.behaviorId w.taskId w.fireKey
  let decision := GoalAutomation.decideTaskFireRecovery expected
    ⟨w.request, w.durableGoalPresent, w.creationClaimPresent⟩
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"agent_did\":" ++ jsonString w.agentDid ++ ","
    ++ "\"behavior_id\":" ++ jsonString w.behaviorId ++ ","
    ++ "\"task_id\":" ++ jsonString w.taskId ++ ","
    ++ "\"fire_key\":" ++ jsonString w.fireKey ++ ","
    ++ "\"request_present\":" ++ boolString w.request.isSome ++ ","
    ++ "\"request_binding_matches\":" ++ boolString (w.request == some expected) ++ ","
    ++ "\"observed_agent_did\":" ++ goalOptionalStringJson (w.request.map (·.agentDid)) ++ ","
    ++ "\"observed_behavior_id\":" ++ goalOptionalStringJson (w.request.map (·.behaviorId)) ++ ","
    ++ "\"observed_session_id\":" ++ goalOptionalStringJson (w.request.map (·.sessionId)) ++ ","
    ++ "\"observed_request_id\":" ++ goalOptionalStringJson (w.request.map (·.requestId)) ++ ","
    ++ "\"observed_retry_key\":" ++ goalOptionalStringJson (w.request.map (·.retryKey)) ++ ","
    ++ "\"durable_goal_present\":" ++ boolString w.durableGoalPresent ++ ","
    ++ "\"creation_claim_present\":" ++ boolString w.creationClaimPresent ++ ","
    ++ "\"expected_disposition\":" ++ jsonString
      (taskFireRecoveryDispositionName decision.disposition) ++ ","
    ++ "\"expected_recovered_request_id\":" ++
      goalOptionalStringJson decision.recoveredRequestId ++ ","
    ++ "\"expected_checkpointable\":" ++ boolString decision.checkpointable
    ++ "}"

def taskGoalRecoveryCasesJson : String :=
  jsonArray (taskGoalRecoveryCases.map taskGoalRecoveryCaseJson)

structure GoalSubmissionCase where
  name : String
  state : GoalAutomation.SubmissionState
  action : GoalAutomation.SubmissionAction

def goalSubmissionCases : List GoalSubmissionCase :=
  [ { name := "stage_goal_not_visible", state := ⟨false, false, false, false⟩,
      action := .stageGoal }
  , { name := "request_cannot_stage_without_goal", state := ⟨false, false, false, false⟩,
      action := .stageRequest }
  , { name := "commit_both_atomically", state := ⟨false, false, true, true⟩,
      action := .commit }
  , { name := "failure_between_writes_discards_staging", state := ⟨false, false, true, false⟩,
      action := .crash }
  , { name := "committed_retry_is_idempotent", state := ⟨true, true, false, false⟩,
      action := .commit }
  ]

def submissionActionName : GoalAutomation.SubmissionAction → String
  | .stageGoal => "stage_goal" | .stageRequest => "stage_request"
  | .commit => "commit" | .abort => "abort" | .crash => "crash"

def goalSubmissionCaseJson (w : GoalSubmissionCase) : String :=
  let post := GoalAutomation.submissionStep w.state w.action
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"durable_goal\":" ++ boolString w.state.durableGoal ++ ","
    ++ "\"runnable_request\":" ++ boolString w.state.runnableRequest ++ ","
    ++ "\"staged_goal\":" ++ boolString w.state.stagedGoal ++ ","
    ++ "\"staged_request\":" ++ boolString w.state.stagedRequest ++ ","
    ++ "\"action\":" ++ jsonString (submissionActionName w.action) ++ ","
    ++ "\"expected_durable_goal\":" ++ boolString post.durableGoal ++ ","
    ++ "\"expected_runnable_request\":" ++ boolString post.runnableRequest ++ ","
    ++ "\"expected_staged_goal\":" ++ boolString post.stagedGoal ++ ","
    ++ "\"expected_staged_request\":" ++ boolString post.stagedRequest
    ++ "}"

def goalSubmissionCasesJson : String :=
  jsonArray (goalSubmissionCases.map goalSubmissionCaseJson)

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

def continuationPhaseName : GoalAutomation.ContinuationPhase → String
  | .unclaimed => "unclaimed" | .claimed => "claimed" | .childPresent => "child_present"

def continuationActionName : GoalAutomation.ContinuationAction → String
  | .claim true => "claim_eligible" | .claim false => "claim_ineligible"
  | .materialize => "materialize" | .reconcile => "reconcile" | .crash => "crash"

def goalContinuationMaterializationCaseJson
    (w : GoalContinuationMaterializationCase) : String :=
  let post := GoalAutomation.continuationStep w.phase w.action
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"phase\":" ++ jsonString (continuationPhaseName w.phase) ++ ","
    ++ "\"action\":" ++ jsonString (continuationActionName w.action) ++ ","
    ++ "\"expected_phase\":" ++ jsonString (continuationPhaseName post)
    ++ "}"

def goalContinuationMaterializationCasesJson : String :=
  jsonArray (goalContinuationMaterializationCases.map goalContinuationMaterializationCaseJson)

end Conformance.Contracts
