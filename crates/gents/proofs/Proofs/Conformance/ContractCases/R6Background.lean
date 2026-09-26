import Proofs.Background.Budget
import Proofs.Background.ToolOutput
import Proofs.Background.CompletionContinuation
import Proofs.Background.ProcessControl
import Proofs.Conformance.ContractCases.Types
import Proofs.Recovery.Sweeps.BackgroundRestart
import Proofs.Session.State
import Proofs.ToolExecution.Executable

namespace Conformance.ContractCases

def r6Case
    (name group action : String)
    (legal : Bool)
    (preLiveCount : Nat)
    (terminalState : String)
    (result reason errorCode queueSource queueKey : Option String := none) :
    R6BackgroundingCase :=
  { name := name
  , group := group
  , action := action
  , legal := legal
  , preLiveCount := preLiveCount
  , maxBackgrounded := Background.maxBackgroundedPerParent
  , awaitMode := "background"
  , terminalState := terminalState
  , result := result
  , reason := reason
  , errorCode := errorCode
  , queueSource := queueSource
  , queueKey := queueKey
  }

/-- Concrete childless native-tool row used to execute the R6 lifecycle.
The production `new_background_tool` constructor starts pending/background;
`start_running` supplies the running/committed shape modeled here. -/
def r6NativeToolFixture
    (awaitMode : ToolExecution.AwaitMode := .background) :
    ToolExecution.ToolCallContext :=
  { callId := 77
  , requestId := 900
  , state := .running
  , operation := .nativeCommand
  , deadline := 100
  , startedAt := some 1
  , currentTime := 10
  , failureClass := none
  , persistence := .committed
  , awaitMode := awaitMode
  }

/-- Execute one native-tool action and project its actual post-state into the
R6 JSON row. No caller supplies `legal`, `terminalState` or mode values. -/
def r6NativeStepCase
    (name actionName : String)
    (pre : ToolExecution.ToolCallContext)
    (action : ToolExecution.ToolCallContext.Action)
    (result reason : Option String := none) : R6BackgroundingCase :=
  let base :=
    r6Case name "native_lifecycle" actionName false 1 "rejected"
      result reason
  match ToolExecution.ToolCallContext.step? pre action with
  | none => base
  | some post =>
      { base with
          legal := true
        , awaitMode := post.awaitMode.toDefraDB
        , terminalState := post.state.toDefraDB
      }

/-- Admission is the executable numeric guard enforced by Rust before
creating another live background row. -/
def r6BudgetCase (name : String) (preLiveCount : Nat) :
    R6BackgroundingCase :=
  let legal := (Background.admitBackground preLiveCount).isSome
  r6Case name "budget" "spawn_process" legal preLiveCount
    (if legal then "running" else "rejected")
    none none
    (if legal then none else some "background_tool_budget_exceeded")

/-- The R6 startup row is computed by the same total restart classifier that
drives `ToolCallLifecycle::recover_all` conformance. -/
def r6RestartCase : R6BackgroundingCase :=
  let row : Recovery.RestartRow :=
    { awaitMode := .background
    , sessionMessage := false
    , parent := .live
    , deadlineExpired := false
    , process := .stopped
    }
  let disposition := Recovery.restartDisposition row
  let notification := row.notification
  { name := "background_recovery_running_live_parent_to_cancelled"
  , group := "recovery"
  , action := disposition.causeContract.getD ""
  , legal := true
  , preLiveCount := 1
  , maxBackgrounded := Background.maxBackgroundedPerParent
  , awaitMode := row.awaitMode.toDefraDB
  , terminalState := disposition.terminalStateContract.getD "running"
  , result := none
  , reason := notification.map (·.notificationReason)
  , errorCode := none
  , queueSource := notification.map (·.queueSource)
  , queueKey := notification.map (fun obligation =>
      obligation.queueKeyPrefix ++ "900")
  }

def r6CompletionQueueCase : R6BackgroundingCase :=
  let source := SessionQueue.QueueSource.backgroundCompletion
  r6Case
    "background_completion_source_writes_canonical_key"
    "queue_source"
    "enqueue_background_completion"
    true
    1
    "completed"
    (some "done")
    none
    none
    (some source.toDefraDB)
    (some (source.toDefraDB ++ ":900"))

/-- The terminal → notification → coalesced wake → claimed continuation row is
computed by the composed executable acceptance model. -/
def r6CompletionContinuationCase : R6BackgroundingCase :=
  let accepted := BackgroundCompletion.canonicalContinuationAccepted
  let completion := BackgroundCompletion.canonicalCompletion
  let wake := BackgroundCompletion.canonicalWake
  r6Case
    "terminal_completion_message_precedes_claimed_continuation"
    "completion_continuation"
    "terminalize_append_notification_enqueue_claim"
    accepted
    1
    completion.toolState.toDefraDB
    (if accepted then some "assistant_wait_precedes_notification" else none)
    (if accepted then some "continuation_claimed" else none)
    none
    (some wake.source.toDefraDB)
    (wake.queueKey.map fun key => wake.source.toDefraDB ++ ":" ++ toString key)

/-- These fields drive actual notification publication and failed-wake
redrive consumers; they do not model-check a duplicate Rust reference machine.
The Goal status is a row input only: Goals and background wakes are
independent, so the owner functions do not read it. -/
def r6GoalOwnerCase (name : String) (goal : Option Goals.Status) : R6BackgroundingCase :=
  let notified := BackgroundCompletion.appendNotification?
    BackgroundCompletion.canonicalCompletion BackgroundCompletion.canonicalWaitReservedTranscript
  let queued := notified.bind fun notification =>
    BackgroundCompletion.enqueueWake? notification BackgroundCompletion.canonicalQueue
  let redrive := BackgroundCompletion.redriveWake? (BackgroundCompletion.failedWakeFixture)
  { r6Case name "completion_continuation_owner" "notify_and_select_continuation_owner"
      notified.isSome 1 "completed" with
    goalStatus := goal.map Goals.Status.toDefraDB
    notificationPersisted := some notified.isSome
    wakeCreated := some queued.isSome
    redriveAllowed := some redrive.isSome }

def wakeSession : AgentSession.Document :=
  { scope := ⟨1, 900, some 2⟩, behavior := 3, createdAt := 0 }
def wakeParent (wake : BackgroundCompletion.FailedWake) : AgentSession.RequestFact :=
  { purpose := .normal, scope := wakeSession.scope, behavior := 3, createdAt := 1,
    observed := ⟨101, wake.requestId, wake.ctx.state⟩ }
def wakeSuccessor : AgentSession.RequestFact :=
  { purpose := .normal, scope := wakeSession.scope, behavior := 3, createdAt := 3, observed := ⟨102, 902, .pending⟩ }
/-- Different requester still blocks stale session-wide recovery. -/
def newerInteractive : AgentSession.RequestFact :=
  { purpose := .normal, scope := { wakeSession.scope with requester := some 99 }, behavior := 3,
    createdAt := 2, observed := ⟨103, 903, .processing⟩ }

def r6FailedWakeRedriveCase
    (name : String)
    (wake : BackgroundCompletion.FailedWake) (latest : Bool := true) : R6BackgroundingCase :=
  let parent := wakeParent wake
  let rows := if latest then [parent] else [parent, newerInteractive]
  let post := (BackgroundCompletion.redriveWakeFromRows? wakeSession rows 101 wake
    wakeSuccessor "wake" 3).map (·.1)
  let legal := post.isSome
  { r6Case
      name
      "completion_redrive"
      "redrive_failed_background_wake"
      legal
      1
      wake.ctx.state.toDefraDB
      (if legal then some "successor_created" else none)
      (if legal then some "bounded_retry" else some "ineligible")
      none
      (some wake.source.toDefraDB)
      (wake.queueKey.map fun key => wake.source.toDefraDB ++ ":" ++ toString key) with
      retryCount := some wake.ctx.retryCount
      maxRetries := some wake.ctx.maxRetries
      postRetryCount := post.map (·.retryCount)
      redriveSourceRequestId := some wake.requestId
      preDepth := some wake.ctx.depth
      postDepth := post.map (·.depth)
      preParentRequestId := wake.ctx.parentRequestId
      postParentRequestId := post.bind (·.parentRequestId)
      preExecutionDeadline := wake.ctx.deadline
      postExecutionDeadline := post.bind (·.deadline)
      retryDelaySeconds := if legal then some (BackgroundCompletion.wakeRetryDelaySeconds wake.ctx.retryCount) else none
      isLatest := some (decide (AgentSession.latest rows 1 900 none = some parent))
  }

def r6WakeAdmissionCase
    (name : String)
    (wake other : BackgroundCompletion.AdmissionCandidate) :
    R6BackgroundingCase :=
  let admitted := BackgroundCompletion.servesBefore wake other
  r6Case
    name
    "completion_admission"
    "rank_pending_background_wake"
    admitted
    1
    "pending"
    none
    (if admitted then some "aged_priority" else some "fifo")
    none
    (some wake.source.toDefraDB)
    (some (wake.source.toDefraDB ++ ":900"))

def r6WakeAcknowledgementCase
    (name : String)
    (snapshot : BackgroundCompletion.WakeAttemptSnapshot)
    (terminalState : RequestState) :
    R6BackgroundingCase :=
  let attempted := snapshot.attemptedBindings
  let acknowledged := snapshot.acknowledgedBindings terminalState
  let completed := terminalState = .completed
  r6Case
    name
    "completion_acknowledgement"
    "snapshot_notification_bindings"
    (decide (if completed then acknowledged = attempted else acknowledged = []))
    attempted.length
    terminalState.toDefraDB
    (some ("attempted=" ++ toString attempted.length ++
      ",acknowledged=" ++ toString acknowledged.length))
    (if completed then some "completed_ack" else some "failed_unacknowledged")
    none
    (some SessionQueue.QueueSource.backgroundCompletion.toDefraDB)
    (some "background_completion:900")

def deliveryCrashAction : BackgroundCompletion.DeliveryCrashPoint → String
  | .beforeClaim => "restart_before_claim"
  | .duringInference => "preserve_live_inference"
  | .duringAcknowledgement => "project_acknowledgement_after_restart"

def deliveryCrashReason : BackgroundCompletion.DeliveryCrashPoint → String
  | .beforeClaim => "pending_reclaim"
  | .duringInference => "owner_still_live"
  | .duringAcknowledgement => "atomic_ack_projection"

def r6WakeFailureBoundaryCase
    (name : String)
    (point : BackgroundCompletion.DeliveryCrashPoint) :
    R6BackgroundingCase :=
  let recovered := BackgroundCompletion.recoverDeliveryCrash point
  r6Case
    name
    "completion_failure_boundary"
    (deliveryCrashAction point)
    (BackgroundCompletion.deliveryCrashRecoveryAccepted point)
    recovered.attemptedBindings.length
    recovered.requestState.toDefraDB
    (some ("attempted=" ++ toString recovered.attemptedBindings.length ++
      ",acknowledged=" ++ toString recovered.acknowledgedBindings.length))
    (some (deliveryCrashReason point))
    none
    (some SessionQueue.QueueSource.backgroundCompletion.toDefraDB)
    (some "background_completion:900")

def r6NoncanonicalQueueSourceCase : R6BackgroundingCase :=
  let noncanonical := "subagent_completion"
  let parsed := SessionQueue.QueueSource.fromDefraDB? noncanonical
  r6Case
    "noncanonical_subagent_completion_source_is_rejected"
    "queue_source"
    "reject_noncanonical_subagent_completion"
    parsed.isNone
    1
    "completed"
    (some "done")
    none
    none
    (some noncanonical)
    none

def processScope
    (requestId sessionId agentDid : String)
    (requesterDid : Option String) : Background.ProcessControl.Scope :=
  { requestId, sessionId, agentDid, requesterDid }

def r6ProcessControlCase
    (name action scenario : String)
    (caller owner : Background.ProcessControl.Scope) : R6BackgroundingCase :=
  r6Case name "process_control_authorization" action
    (Background.ProcessControl.authorized caller owner)
    1 "running" none (some scenario)

def r6WaitBoundaryCase
    (name : String) (boundary : Background.ProcessControl.WaitBoundary) :
    R6BackgroundingCase :=
  let observation :=
    Background.ProcessControl.observeBoundary ToolExecution.ToolCallState.running boundary
  r6Case name "wait_boundary" "wait_process"
    (!observation.cancellationRequested)
    1 (ToolExecution.ToolCallState.toDefraDB observation.processState)
    none (some observation.reason)

/-- The standalone wait observation describes why polling stopped, while the
accepted foreground wait call remains owned by its caller request. Once that
request's deadline is authoritative, the existing tool timeout transition
terminalizes the wait call; it never changes the separately backgrounded
process observed above. -/
def r6CallerDeadlineDispatchCase : R6BackgroundingCase :=
  let observation := Background.ProcessControl.observeBoundary
    ToolExecution.ToolCallState.running .callerDeadline
  let waitCall := { r6NativeToolFixture .foreground with
    deadline := 10, currentTime := 11 }
  let base := r6Case
    "caller_deadline_times_out_wait_call_preserves_background_process"
    "wait_dispatch_boundary" "wait_process"
    false 1 "rejected" none (some observation.reason)
  match ToolExecution.ToolCallContext.step? waitCall .timeout with
  | none => base
  | some callerPost =>
      { base with
          legal := !observation.cancellationRequested
          awaitMode := callerPost.awaitMode.toDefraDB
          terminalState := callerPost.state.toDefraDB
          result := some (ToolExecution.ToolCallState.toDefraDB observation.processState) }

/-- An observational wait does not cancel the background process. The caller's
own accepted foreground wait can nevertheless lose the request-interruption
race through the existing Running tool cancellation transition. This case
describes that branch, not a required winner against observer completion. -/
def r6CallerInterruptDispatchCase : R6BackgroundingCase :=
  let observation := Background.ProcessControl.observeBoundary
    ToolExecution.ToolCallState.running .callerInterrupted
  let waitCall := r6NativeToolFixture .foreground
  let base := r6Case
    "caller_interrupt_cancels_wait_call_preserves_background_process"
    "wait_dispatch_boundary" "wait_process"
    false 1 "rejected" none (some observation.reason)
  match ToolExecution.ToolCallContext.step? waitCall
      (.cancelDuringRun .interrupted) with
  | none => base
  | some callerPost =>
      { base with
          legal := !observation.cancellationRequested
          awaitMode := callerPost.awaitMode.toDefraDB
          terminalState := callerPost.state.toDefraDB
          result := some (ToolExecution.ToolCallState.toDefraDB observation.processState) }

theorem r6_caller_interrupt_dispatch_uses_both_existing_owners :
    (r6CallerInterruptDispatchCase.legal,
      r6CallerInterruptDispatchCase.terminalState,
      r6CallerInterruptDispatchCase.result,
      r6CallerInterruptDispatchCase.reason) =
    (true, "cancelled", some "running", some "caller_interrupted") := by
  rfl

/-- The observer may complete its foreground wait before the caller's
interruption sweep cancels that call. Completion belongs to the wait call;
the observed background process remains running. This is the other possible
owner ordering, not a rule selecting a winner. -/
def r6CallerInterruptObserverCompletionCase : R6BackgroundingCase :=
  let observation := Background.ProcessControl.observeBoundary
    ToolExecution.ToolCallState.running .callerInterrupted
  let waitCall := r6NativeToolFixture .foreground
  let base := r6Case
    "caller_interrupt_observer_completes_wait_call_preserves_background_process"
    "wait_dispatch_boundary" "wait_process"
    false 1 "rejected" none (some observation.reason)
  match ToolExecution.ToolCallContext.step? waitCall .complete with
  | none => base
  | some callerPost =>
      { base with
          legal := !observation.cancellationRequested
          awaitMode := callerPost.awaitMode.toDefraDB
          terminalState := callerPost.state.toDefraDB
          result := some (ToolExecution.ToolCallState.toDefraDB observation.processState) }

theorem r6_caller_interrupt_observer_completion_uses_both_existing_owners :
    (r6CallerInterruptObserverCompletionCase.legal,
      r6CallerInterruptObserverCompletionCase.terminalState,
      r6CallerInterruptObserverCompletionCase.result,
      r6CallerInterruptObserverCompletionCase.reason) =
    (true, "completed", some "running", some "caller_interrupted") := by
  rfl

def r6BackgroundingCases : List R6BackgroundingCase :=
  [ r6BudgetCase
      "background_tool_budget_count_7_admits_spawn"
      7
  , r6BudgetCase
      "background_tool_budget_count_8_rejects_spawn"
      8
  , r6NativeStepCase
      "tool_kind_background_mode_executes"
      "background"
      (r6NativeToolFixture .foreground)
      .background
  , r6NativeStepCase
      "tool_kind_bridge_complete_persists_result"
      "complete"
      r6NativeToolFixture
      .complete
      (some "done")
  , r6NativeStepCase
      "tool_kind_explicit_cancel_projects_explicit_cancel"
      "cancel_during_run"
      r6NativeToolFixture
      (.cancelDuringRun .userCancelled)
      none
      (some "explicit_cancel")
  , r6RestartCase
  , r6CompletionQueueCase
  , r6CompletionContinuationCase
  , r6GoalOwnerCase "no_goal_preserves_background_wake" none
  , r6GoalOwnerCase "active_goal_keeps_background_wake" (some .active)
  , r6GoalOwnerCase "paused_goal_keeps_background_wake" (some .paused)
  , r6GoalOwnerCase "blocked_goal_keeps_background_wake" (some .blocked)
  , r6GoalOwnerCase "usage_limited_goal_keeps_background_wake" (some .usageLimited)
  , r6GoalOwnerCase "budget_limited_goal_keeps_background_wake" (some .budgetLimited)
  , r6GoalOwnerCase "complete_goal_keeps_background_wake" (some .complete)
  , r6FailedWakeRedriveCase
      "failed_background_wake_with_budget_redrives"
      (BackgroundCompletion.failedWakeFixture (retryCount := 1))
  , r6FailedWakeRedriveCase
      "failed_background_wake_exhausted_budget_stops"
      (BackgroundCompletion.failedWakeFixture (retryCount := 3))
  , r6FailedWakeRedriveCase
      "generic_scheduled_failure_is_not_background_redrive"
      (BackgroundCompletion.failedWakeFixture (source := .user))
  , r6FailedWakeRedriveCase
      "non_latest_background_wake_does_not_redrive"
      (BackgroundCompletion.failedWakeFixture) false
  , r6WakeAdmissionCase
      "aged_background_wake_precedes_new_descendant"
      BackgroundCompletion.agedWakeFixture
      BackgroundCompletion.descendantFixture
  , r6WakeAdmissionCase
      "fresh_background_wake_preserves_fifo"
      BackgroundCompletion.freshWakeFixture
      BackgroundCompletion.descendantFixture
  , r6WakeAcknowledgementCase
      "completed_wake_acknowledges_exact_claim_snapshot"
      BackgroundCompletion.canonicalSnapshotFixture .completed
  , r6WakeAcknowledgementCase
      "failed_wake_retains_claim_snapshot_unacknowledged"
      BackgroundCompletion.canonicalSnapshotFixture .failed
  , r6WakeFailureBoundaryCase
      "restart_before_claim_preserves_pending_notification"
      .beforeClaim
  , r6WakeFailureBoundaryCase
      "live_inference_retains_snapshot_without_ack_or_redrive"
      .duringInference
  , r6WakeFailureBoundaryCase
      "acknowledgement_projection_restart_is_atomic"
      .duringAcknowledgement
  , r6NoncanonicalQueueSourceCase
  , r6ProcessControlCase
      "list_processes_same_requester_next_turn_authorized"
      "list_processes" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "read_process_same_requester_next_turn_authorized"
      "read_process" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "wait_process_same_requester_next_turn_authorized"
      "wait_process" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "cancel_process_same_requester_next_turn_authorized"
      "cancel_process" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "originating_request_without_matching_requester_is_denied"
      "read_process" "originating_request_missing_requester"
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" none)
  , r6ProcessControlCase
      "absent_requester_next_turn_authorized"
      "read_process" "absent_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" none)
      (processScope "request-1" "session-1" "did:agent" none)
  , r6ProcessControlCase
      "empty_requester_does_not_alias_absent"
      "read_process" "empty_requester_vs_absent"
      (processScope "request-2" "session-1" "did:agent" (some ""))
      (processScope "request-1" "session-1" "did:agent" none)
  , r6ProcessControlCase
      "process_control_cross_session_denied"
      "cancel_process" "cross_session"
      (processScope "request-2" "session-2" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "process_control_cross_agent_denied"
      "wait_process" "cross_agent"
      (processScope "request-2" "session-1" "did:other" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "process_control_cross_requester_denied"
      "list_processes" "cross_requester"
      (processScope "request-2" "session-1" "did:agent" (some "did:other"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6WaitBoundaryCase
      "wait_timeout_preserves_running_process"
      .waitTimeout
  , r6WaitBoundaryCase
      "caller_interrupt_preserves_running_process"
      .callerInterrupted
  , r6WaitBoundaryCase
      "caller_deadline_preserves_running_process"
      .callerDeadline
  , r6CallerDeadlineDispatchCase
  , r6CallerInterruptDispatchCase
  , r6CallerInterruptObserverCompletionCase
  ]

/-- Pin the concrete projections while keeping their construction executable:
changing the mode guard, native completion/cancel state, budget, restart
classifier, or queue vocabulary changes this tuple and fails `lake build`. -/
theorem r6BackgroundingCases_pinned :
    r6BackgroundingCases.map
        (fun witness =>
          (witness.name, witness.legal, witness.awaitMode,
            witness.terminalState,
            witness.queueSource, witness.queueKey)) =
      [ ("background_tool_budget_count_7_admits_spawn", true, "background",
          "running", none, none)
      , ("background_tool_budget_count_8_rejects_spawn", false, "background",
          "rejected", none, none)
      , ("tool_kind_background_mode_executes", true, "background",
          "running", none, none)
      , ("tool_kind_bridge_complete_persists_result", true, "background",
          "completed", none, none)
      , ("tool_kind_explicit_cancel_projects_explicit_cancel",
          true, "background", "cancelled", none, none)
      , ("background_recovery_running_live_parent_to_cancelled", true,
          "background", "cancelled", some "background_completion",
          some "background_completion:900")
      , ("background_completion_source_writes_canonical_key", true,
          "background", "completed", some "background_completion",
          some "background_completion:900")
      , ("terminal_completion_message_precedes_claimed_continuation", true,
          "background", "completed", some "background_completion",
          some "background_completion:900")
      , ("no_goal_preserves_background_wake", true, "background", "completed", none, none)
      , ("active_goal_keeps_background_wake", true, "background", "completed", none, none)
      , ("paused_goal_keeps_background_wake", true, "background", "completed", none, none)
      , ("blocked_goal_keeps_background_wake", true, "background", "completed", none, none)
      , ("usage_limited_goal_keeps_background_wake", true, "background", "completed", none, none)
      , ("budget_limited_goal_keeps_background_wake", true, "background", "completed", none, none)
      , ("complete_goal_keeps_background_wake", true, "background", "completed", none, none)
      , ("failed_background_wake_with_budget_redrives", true,
          "background", "failed", some "background_completion",
          some "background_completion:900")
      , ("failed_background_wake_exhausted_budget_stops", false,
          "background", "failed", some "background_completion",
          some "background_completion:900")
      , ("generic_scheduled_failure_is_not_background_redrive", false,
          "background", "failed", some "user", some "user:900")
      , ("non_latest_background_wake_does_not_redrive", false,
          "background", "failed", some "background_completion",
          some "background_completion:900")
      , ("aged_background_wake_precedes_new_descendant", true,
          "background", "pending", some "background_completion",
          some "background_completion:900")
      , ("fresh_background_wake_preserves_fifo", false,
          "background", "pending", some "background_completion",
          some "background_completion:900")
      , ("completed_wake_acknowledges_exact_claim_snapshot", true,
          "background", "completed", some "background_completion",
          some "background_completion:900")
      , ("failed_wake_retains_claim_snapshot_unacknowledged", true,
          "background", "failed", some "background_completion",
          some "background_completion:900")
      , ("restart_before_claim_preserves_pending_notification", true,
          "background", "pending", some "background_completion",
          some "background_completion:900")
      , ("live_inference_retains_snapshot_without_ack_or_redrive", true,
          "background", "processing", some "background_completion",
          some "background_completion:900")
      , ("acknowledgement_projection_restart_is_atomic", true,
          "background", "completed", some "background_completion",
          some "background_completion:900")
      , ("noncanonical_subagent_completion_source_is_rejected", true,
          "background", "completed", some "subagent_completion",
          none)
      , ("list_processes_same_requester_next_turn_authorized", true,
          "background", "running", none, none)
      , ("read_process_same_requester_next_turn_authorized", true,
          "background", "running", none, none)
      , ("wait_process_same_requester_next_turn_authorized", true,
          "background", "running", none, none)
      , ("cancel_process_same_requester_next_turn_authorized", true,
          "background", "running", none, none)
      , ("originating_request_without_matching_requester_is_denied", false,
          "background", "running", none, none)
      , ("absent_requester_next_turn_authorized", true,
          "background", "running", none, none)
      , ("empty_requester_does_not_alias_absent", false,
          "background", "running", none, none)
      , ("process_control_cross_session_denied", false,
          "background", "running", none, none)
      , ("process_control_cross_agent_denied", false,
          "background", "running", none, none)
      , ("process_control_cross_requester_denied", false,
          "background", "running", none, none)
      , ("wait_timeout_preserves_running_process", true,
          "background", "running", none, none)
      , ("caller_interrupt_preserves_running_process", true,
          "background", "running", none, none)
      , ("caller_deadline_preserves_running_process", true,
          "background", "running", none, none)
      , ("caller_deadline_times_out_wait_call_preserves_background_process", true,
          "foreground", "timedOut", none, none)
      , ("caller_interrupt_cancels_wait_call_preserves_background_process", true,
          "foreground", "cancelled", none, none)
      , ("caller_interrupt_observer_completes_wait_call_preserves_background_process", true,
          "foreground", "completed", none, none)
      ] := by
  rfl

/-- Pin ownership inputs and durable outcomes, not only the older common
R6 fields. Goals and background wakes are independent: every emitted case
retains its notification, enqueues the wake and allows failed-wake redrive,
whatever the Goal status. -/
theorem goal_owner_delivery_cases_pin_all_outcomes :
    ((r6BackgroundingCases.filter fun c => c.group == "completion_continuation_owner").map
      fun c => (c.name, c.goalStatus, c.notificationPersisted, c.wakeCreated, c.redriveAllowed)) =
      [ ("no_goal_preserves_background_wake", none, some true, some true, some true)
      , ("active_goal_keeps_background_wake", some "active", some true, some true, some true)
      , ("paused_goal_keeps_background_wake", some "paused", some true, some true, some true)
      , ("blocked_goal_keeps_background_wake", some "blocked", some true, some true, some true)
      , ("usage_limited_goal_keeps_background_wake", some "usage_limited", some true, some true, some true)
      , ("budget_limited_goal_keeps_background_wake", some "budget_limited", some true, some true, some true)
      , ("complete_goal_keeps_background_wake", some "complete", some true, some true, some true) ] := by
  rfl

/-! ## Tool output paging witnesses (#937)

Outputs are computed from `Background.ToolOutput.readSlice`; the pinned tuple
theorem below fails at Lean build time if the slice model drifts, and the
Rust `background_tools` unit test fails if `read_retained_output_slice`
drifts from the emitted rows. -/

def toolOutputPagingCase
    (name : String)
    (retainedLen totalBytes offset maxBytes : Nat)
    (theoremName : String) : ToolOutputPagingCase :=
  let firstOffset := 0
  let window : Background.ToolOutput.RetainedWindow :=
    { firstOffset := firstOffset
    , retainedLen := retainedLen
    , totalBytes := totalBytes
    }
  let slice := Background.ToolOutput.readSlice window offset maxBytes
  { name := name
  , firstOffset := firstOffset
  , retainedLen := retainedLen
  , totalBytes := totalBytes
  , offset := offset
  , maxBytes := maxBytes
  , start := slice.start
  , sliceLen := slice.sliceLen
  , nextOffset := slice.nextOffset
  , firstAvailableOffset := slice.firstAvailableOffset
  , totalBytesOut := slice.totalBytes
  , hasMore := slice.hasMore
  , theoremName := theoremName
  }

def toolOutputPagingCases : List ToolOutputPagingCase :=
  [ toolOutputPagingCase "paging_head_page" 8 8 0 4
      "Background.ToolOutput.readSlice_contiguous_from_live_cursor"
  , toolOutputPagingCase "paging_continuation_no_gap" 8 8 4 4
      "Background.ToolOutput.readSlice_contiguous_from_live_cursor"
  , toolOutputPagingCase "paging_cursor_past_end_parks" 4 4 9 4
      "Background.ToolOutput.readSlice_past_end_empty"
  , toolOutputPagingCase "paging_mid_window_bounded_budget" 7 7 3 2
      "Background.ToolOutput.readSlice_progress"
  ]

/-- Pinned expected outputs: fails at Lean build time if `readSlice` drifts,
    keeping the emitted rows honest rather than self-referential. -/
theorem toolOutputPagingCases_pinned :
    toolOutputPagingCases.map
        (fun witness =>
          (witness.name, witness.start, witness.sliceLen, witness.nextOffset,
            witness.firstAvailableOffset, witness.totalBytesOut,
            witness.hasMore)) =
      [ ("paging_head_page", 0, 4, 4, 0, 8, true)
      , ("paging_continuation_no_gap", 4, 4, 8, 0, 8, false)
      , ("paging_cursor_past_end_parks", 4, 0, 4, 0, 4, false)
      , ("paging_mid_window_bounded_budget", 3, 2, 5, 0, 7, true)
      ] := by
  rfl

/-- The two contiguous pages tile the window with no gap and no overlap. -/
theorem toolOutputPagingCases_head_and_continuation_tile :
    ∀ head ∈ toolOutputPagingCases.filter
        (fun witness => witness.name = "paging_head_page"),
      ∀ next ∈ toolOutputPagingCases.filter
          (fun witness => witness.name = "paging_continuation_no_gap"),
        head.nextOffset = next.offset ∧ head.nextOffset = next.start := by
  native_decide

def r6BackgroundTheoremWitnesses : List BackgroundTheoremWitness :=
  [ { theoremName := "Background.admitted_background_count_bounded"
    , witnessKind := "admission_bound"
    , scenario := "background_tool_admission_respects_max_backgrounded_per_parent"
    , numericBound := Background.maxBackgroundedPerParent
    , kindFields :=
        [ ("await_mode", "background")
        , ("error_code_on_violation", "background_tool_budget_exceeded")
        ]
    }
  ]

/-- Full row inputs for the composed transaction gate, not a supplied isLatest
flag. Publication output carries both successor rows and the session observation. -/
structure WakeRowsCase where
  name : String
  session : AgentSession.Document := wakeSession
  rows : List AgentSession.RequestFact := [wakeParent BackgroundCompletion.failedWakeFixture]
  parentDoc : Nat := 101
  wake : BackgroundCompletion.FailedWake := BackgroundCompletion.failedWakeFixture
  successor : AgentSession.RequestFact := wakeSuccessor

def WakeRowsCase.result (c : WakeRowsCase) :=
  BackgroundCompletion.redriveWakeFromRows? c.session c.rows c.parentDoc c.wake
    c.successor "wake" 3

def backgroundWakeRowsCases : List WakeRowsCase :=
  [ { name := "head-wake-publishes-successor-and-observation" }
  , { name := "newer-interactive-requester-blocks-stale-wake",
      rows := [wakeParent BackgroundCompletion.failedWakeFixture, newerInteractive] }
  , { name := "wrong-physical-parent-denied", parentDoc := 999 }
  , { name := "successor-physical-id-collision-denied",
      successor := { wakeSuccessor with observed := ⟨101, 902, .pending⟩ } }
  , { name := "successor-logical-id-collision-denied",
      successor := { wakeSuccessor with observed := ⟨102, 901, .pending⟩ } }
  , { name := "older-successor-cannot-advance-head",
      successor := { wakeSuccessor with createdAt := 0 } }
  , { name := "published-successor-prevents-second-redrive",
      rows := [wakeParent BackgroundCompletion.failedWakeFixture, wakeSuccessor],
      successor := { wakeSuccessor with createdAt := 4, observed := ⟨104, 904, .pending⟩ } }
  , { name := "stale-session-observation-does-not-block-current-parent",
      session := { wakeSession with observation := some ⟨2, none, some ⟨999, 999, .processing⟩⟩ } }
  , { name := "wrong-session-queue-key-denied",
      wake := BackgroundCompletion.failedWakeFixture (queueKey := some 999) }
  ]

theorem background_recovery_rows_pinned :
    backgroundWakeRowsCases.map (fun c => c.result.isSome) =
      [true, false, false, false, false, false, false, true, false] := by native_decide

theorem background_successor_publication_pinned :
    ((WakeRowsCase.result { name := "publication" }).map fun post =>
      (post.1.retryCount, post.2.1.observation.bind (·.latest),
       post.2.2.map (·.observed.docId))) =
      some (1, some wakeSuccessor.observed, [101, 102]) := by native_decide

end Conformance.ContractCases
