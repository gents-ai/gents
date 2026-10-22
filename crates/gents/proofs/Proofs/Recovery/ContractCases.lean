import Proofs.Recovery.Sweeps
import Proofs.Conformance.ContractCases
import Proofs.Conformance.PluginJob

namespace Recovery

open Conformance.ContractCases

def recoveryCase
    (sweep : RecoverySweep)
    (name preState terminalState deadlineAuditRef : String)
    (measureBefore : Nat := 1)
    (measureAfter : Nat := 0) : RecoverySweepCase :=
  { name := name
  , sweepId := sweep.sweepId
  , collection := sweep.collection.toContract
  , rustFunction := sweep.rustFunction
  , cadence := sweep.cadence.toContract
  , implementationStatus := sweep.implementationStatus.toContract
  , preState := preState
  , terminalState := terminalState
  , measureBefore := measureBefore
  , measureAfter := measureAfter
  , deadlineAuditRef := deadlineAuditRef
  }

def orphanedBackgroundRecoveryCase
    (name : String)
    (deadlineExpired parentLive parentInterrupted
      parentTerminal executionRegistered : Bool)
    (process : ManagedExec.StopOutcome := .stopped)
    (ownerTaskDeleted : Bool := false)
    (ownerSessionStopped : Bool := false) : RecoverySweepCase :=
  let row : OrphanedBackgroundToolRow :=
    { call := r6NativeToolFixture
    , deadlineExpired := deadlineExpired
    , parentLive := parentLive
    , parentInterrupted := parentInterrupted
    , parentTerminal := parentTerminal
    , executionRegistered := executionRegistered
    , process := process
    , ownerSessionStopped := ownerSessionStopped
    , ownerTaskDeleted := ownerTaskDeleted
    }
  let recovered := orphanedBackgroundToolRecover row
  let cause := orphanedBackgroundToolCause row
  let notificationReason :=
    if row.parentLive || row.parentInterrupted || row.parentTerminal then
      cause.bind ToolRecoveryCause.notificationReason
    else
      none
  { (recoveryCase
      orphanedBackgroundToolSweep
      name
      row.call.state.toDefraDB
      recovered.call.state.toDefraDB
      "r6-cross-turn-background-process-durability"
      (orphanedBackgroundToolMeasure row)
      (orphanedBackgroundToolMeasure recovered)) with
    deadlineExpired := some row.deadlineExpired
    parentLive := some row.parentLive
    parentInterrupted := some row.parentInterrupted
    parentTerminal := some row.parentTerminal
    executionRegistered := some row.executionRegistered
    processOutcome := some row.process.toContract
    ownerSessionStopped := some row.ownerSessionStopped
    ownerTaskDeleted := some row.ownerTaskDeleted
    recoveryCause := cause.map ToolRecoveryCause.toContract
    notificationReason := notificationReason
  }

def sessionMessageRecoveryCase
    (name : String) (cause : ToolRecoveryCause)
    (handoff : Recovery.AssignmentHandoff := .complete) : RecoverySweepCase :=
  let row : ToolCallRecoveryRow :=
    { call := { r6NativeToolFixture with operation := .sessionMessage }
    , cause := cause }
  let recovered := toolCallRecover row
  { (recoveryCase
      sessionMessageRecoverySweep
      name
      row.call.state.toDefraDB
      recovered.call.state.toDefraDB
      "session-message-stack-observer-arm"
      (toolCallRecoveryMeasure .sessionMessage row)
      (toolCallRecoveryMeasure .sessionMessage recovered)) with
    recoveryCause := some cause.toContract
    notificationReason :=
      if cause.toContract = "requestCompleted" then
        Recovery.sessionMessageHandoffReason true handoff
      else
        cause.notificationReason
  }

/-- Plugin-job recovery witness: one tick of the registered sweep over a
    running row; a row the tick leaves not running has failed closed. -/
def pluginJobRecoveryCase (c : Conformance.PluginJob.RecoveryCase) : RecoverySweepCase :=
  let after := Conformance.PluginJob.recoveryTick c.row
  let state := fun (row : PluginJobRow) =>
    (if row.running then ToolExecution.ToolCallState.running
      else if row.sessionStopped then .cancelled else .failed).toDefraDB
  { (recoveryCase
      pluginJobRecoverySweep
      c.name
      (state c.row)
      (state after)
      "plugin-job-recovery-tick"
      (Conformance.PluginJob.recoveryMeasure c.row)
      (Conformance.PluginJob.recoveryMeasure after)) with
    deadlineExpired := some c.row.deadlineExpired
  }

def taskHookRecordRecoveryCase
    (name : String) (requestResolved liveExecution : Bool) : RecoverySweepCase :=
  let row : TaskHookRecordRow :=
    { recorded := true, requestResolved := requestResolved, liveExecution := liveExecution }
  let recovered := taskHookRecordRecover row
  let state := fun (row : TaskHookRecordRow) => if row.recorded then "running" else "released"
  { (recoveryCase
      taskHookRecoverySweep
      name
      (state row)
      (state recovered)
      "gents-1956-task-hook-cleanup-recovery"
      (taskHookRecordMeasure row)
      (taskHookRecordMeasure recovered)) with
    parentTerminal := some requestResolved
    executionRegistered := some liveExecution
  }

def recoverySweepCases : List RecoverySweepCase :=
  [ recoveryCase
      requestRecoverySweep
      "request_claimed_recovery_to_failed"
      "claimed"
      "failed"
      "formal-coverage-audit-2026-05-13-gap-6"
  , recoveryCase
      requestRecoverySweep
      "request_processing_recovery_to_failed"
      "processing"
      "failed"
      "formal-coverage-audit-2026-05-13-gap-6"
  , recoveryCase
      requestRecoverySweep
      "request_processing_interrupted_recovery_to_interrupted"
      "processing"
      "interrupted"
      "canonical-output-expired-generation-recovery"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_deadline_exceeded_to_timed_out"
      "running"
      "timedOut"
      "deadline-plumbing-audit-2026-05-12-tool-call-persisted-deadline"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_parent_interrupted_to_cancelled"
      "running"
      "cancelled"
      "deadline-plumbing-audit-2026-05-12-request-interrupt-lifetime"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_terminal_parent_to_failed"
      "running"
      "failed"
      "deadline-plumbing-audit-2026-05-12-tool-call-persisted-deadline"
  , recoveryCase
      terminalParentOwnedToolSweep
      "live_running_native_tool_parent_interrupted_to_cancelled"
      "running"
      "cancelled"
      "terminal-parent-owned-tool-cleanup"
  , recoveryCase
      terminalParentOwnedToolSweep
      "live_running_tool_parent_terminal_to_failed"
      "running"
      "failed"
      "terminal-parent-owned-tool-cleanup"
  , recoveryCase
      toolCallRecoverySweep
      "tool_backgrounded_running_unowned_process_to_failed"
      "running"
      "failed"
      "gents-1858-process-ownership"
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_without_execution_to_cancelled"
      false true false false false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_expired_missing_parent_deferred"
      true false false false false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_expired_terminal_parent_to_timed_out"
      true false false true false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_terminal_parent_to_cancelled"
      false false false true false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_unowned_process_to_failed"
      false true false false false (process := .notOwned)
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_exited_process_to_failed"
      false false false true false (process := .alreadyExited)
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_still_running_deferred"
      false true false false false (process := .stillRunning)
  , orphanedBackgroundRecoveryCase
      "registered_background_tool_left_to_worker_deferred"
      false true false false true
  , orphanedBackgroundRecoveryCase
      "registered_background_tool_task_deleted_to_cancelled"
      false true false false true (ownerTaskDeleted := true)
  , orphanedBackgroundRecoveryCase
      "registered_background_tool_session_stopped_to_cancelled"
      false true false false true (ownerSessionStopped := true)
  , orphanedBackgroundRecoveryCase
      "registered_background_tool_session_stop_still_running_deferred"
      false true false false true (process := .stillRunning)
      (ownerSessionStopped := true)
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_session_stopped_to_cancelled"
      false false false true false (ownerSessionStopped := true)
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_interrupted_parent_to_cancelled"
      false false true false false
  , recoveryCase
      backgroundCompletionSideEffectSweep
      "terminal_background_tool_missing_completion_side_effects_to_converged"
      "failed"
      "failed"
      "r6-cross-turn-background-process-durability"
  , sessionMessageRecoveryCase
      "session_message_caused_request_unbound_to_failed" .causedRequestUnbound
  , sessionMessageRecoveryCase
      "session_message_request_completed_to_completed" .requestCompleted
  , sessionMessageRecoveryCase
      "session_message_request_completed_no_goal_to_completed" .requestCompleted
      .absent
  , sessionMessageRecoveryCase
      "session_message_request_completed_unknown_goal_to_completed" .requestCompleted
      .unknown
  , sessionMessageRecoveryCase
      "session_message_request_completed_live_goal_to_completed" .requestCompleted
      (.live "active")
  , sessionMessageRecoveryCase
      "session_message_request_completed_blocked_goal_to_completed" .requestCompleted
      (.live "blocked")
  , sessionMessageRecoveryCase
      "session_message_request_completed_stopped_goal_to_completed" .requestCompleted
      .stopped
  , sessionMessageRecoveryCase
      "session_message_request_failed_to_failed" .requestFailed
  , sessionMessageRecoveryCase
      "session_message_request_interrupted_to_cancelled" .requestInterrupted
  , sessionMessageRecoveryCase
      "session_message_request_dead_to_failed" .requestDead
  , sessionMessageRecoveryCase
      "session_message_request_superseded_to_failed" .requestSuperseded
  ] ++ Conformance.PluginJob.recoveryCases.map pluginJobRecoveryCase ++
  [ recoveryCase
      inferenceCallRecoverySweep
      "inference_queued_stale_to_cancelled"
      "queued"
      "cancelled"
      "deadline-plumbing-audit-2026-05-12-follow-up-6-pr-e"
  , recoveryCase
      inferenceCallRecoverySweep
      "inference_running_stale_to_failed"
      "running"
      "failed"
      "deadline-plumbing-audit-2026-05-12-follow-up-6-pr-e"
  , recoveryCase
      inferenceCallRecoverySweep
      "inference_interrupted_parent_to_cancelled"
      "running"
      "cancelled"
      "deadline-plumbing-audit-2026-05-12-follow-up-6-pr-e"
  , taskHookRecordRecoveryCase
      "task_hook_record_resolved_request_to_released" true false
  , taskHookRecordRecoveryCase
      "task_hook_record_owned_request_deferred" false false
  , taskHookRecordRecoveryCase
      "task_hook_record_live_execution_deferred" true true
  ]

def restartDispositionCase
    (name : String)
    (awaitMode : ToolExecution.AwaitMode)
    (kind : RowKind)
    (parent : ParentObservation)
    (theoremName : String)
    (deadlineExpired : Bool := false)
    (process : ManagedExec.StopOutcome := .stopped) : RestartDispositionCase :=
  let row : RestartRow :=
    { awaitMode := awaitMode
    , kind := kind
    , parent := parent
    , deadlineExpired := deadlineExpired
    , process := process
    }
  let disposition := restartDisposition row
  { name := name
  , rustFunction := "ToolCallLifecycle::recover_all"
  , awaitMode := awaitMode.toDefraDB
  , kind := kind
  , parentObservation := parent.toContract
  , deadlineExpired := deadlineExpired
  , processOutcome := process.toContract
  , disposition := disposition.toContract
  , cause := disposition.causeContract
  , terminalState := disposition.terminalStateContract
  , notificationReason :=
      row.notification.map RestartNotificationObligation.notificationReason
  , queueSource :=
      row.notification.map RestartNotificationObligation.queueSource
  , queueKeyPrefix :=
      row.notification.map RestartNotificationObligation.queueKeyPrefix
  , theoremName := theoremName
  }

def restartDispositionCases : List RestartDispositionCase :=
  [ restartDispositionCase
      "restart_native_background_live_parent_interrupted"
      .background .native .live
      "Recovery.native_background_tool_live_parent_interrupted_on_restart"
  , restartDispositionCase
      "restart_native_background_unowned_process_lost"
      .background .native .live
      "Recovery.native_background_unstopped_process_settles_lost"
      (process := .notOwned)
  , restartDispositionCase
      "restart_native_background_exited_process_lost"
      .background .native .otherTerminal
      "Recovery.native_background_unstopped_process_settles_lost"
      (process := .alreadyExited)
  , restartDispositionCase
      "restart_native_background_still_running_left_running"
      .background .native .live
      "Recovery.native_background_still_running_left_running"
      (process := .stillRunning)
  , restartDispositionCase
      "restart_session_message_live_parent_left_running"
      .background .sessionMessage .live
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_interrupted_parent_left_running"
      .background .sessionMessage .interrupted
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_failed_parent_left_running"
      .background .sessionMessage .otherTerminal
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_clean_complete_parent_left_running"
      .background .sessionMessage .cleanlyCompleted
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_deadline_expired_left_running"
      .background .sessionMessage .live
      "Recovery.session_message_row_left_running"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_deadline_expired_times_out"
      .background .native .live
      "Recovery.deadline_precedes_restart_interrupt"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_interrupted_parent_lost_on_restart"
      .background .native .interrupted
      "Recovery.native_background_tool_interrupted_on_restart"
  , restartDispositionCase
      "restart_native_background_terminal_parent_lost_on_restart"
      .background .native .otherTerminal
      "Recovery.native_background_tool_interrupted_on_restart"
  , restartDispositionCase
      "restart_foreground_interrupted_parent_cancelled"
      .foreground .native .interrupted
      "Recovery.leave_running_iff_preserved_shapes"
  , restartDispositionCase
      "restart_foreground_live_parent_left_running"
      .foreground .native .live
      "Recovery.leave_running_iff_preserved_shapes"
  , restartDispositionCase
      "restart_session_message_missing_parent_left_running"
      .background .sessionMessage .missing
      "Recovery.missing_parent_never_terminalizes"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_expired_missing_parent_deferred"
      .background .native .missing
      "Recovery.missing_parent_never_terminalizes"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_plugin_job_live_parent_left_running"
      .background .pluginJob .live
      "Recovery.plugin_job_row_left_running"
  , restartDispositionCase
      "restart_plugin_job_interrupted_parent_left_running"
      .background .pluginJob .interrupted
      "Recovery.plugin_job_row_left_running"
  , restartDispositionCase
      "restart_plugin_job_deadline_expired_left_running"
      .background .pluginJob .otherTerminal
      "Recovery.plugin_job_row_left_running"
      (deadlineExpired := true)
  ]

theorem restartDispositionCases_cover_every_disposition :
    (restartDispositionCases.filter
        (fun witness => witness.disposition = "leave_running")).length = 12 ∧
      (restartDispositionCases.filter
        (fun witness => witness.disposition = "terminalize")).length = 7 := by
  native_decide

theorem restartDispositionCases_notifications_pinned :
    (restartDispositionCases.filter
        (fun witness => witness.notificationReason.isSome)).map
        (fun witness =>
          (witness.name, witness.notificationReason, witness.queueSource,
            witness.queueKeyPrefix)) =
      [ ("restart_native_background_live_parent_interrupted"
        , some "interrupted_on_restart"
        , some "background_completion"
        , some "background_completion:"
        )
      , ("restart_native_background_unowned_process_lost"
        , some "process_lost"
        , some "background_completion"
        , some "background_completion:"
        )
      , ("restart_native_background_exited_process_lost"
        , some "process_lost"
        , some "background_completion"
        , some "background_completion:"
        )
      , ("restart_native_background_deadline_expired_times_out"
        , some "deadline_exceeded"
        , some "background_completion"
        , some "background_completion:"
        )
      , ("restart_native_background_interrupted_parent_lost_on_restart"
        , some "interrupted_on_restart"
        , some "background_completion"
        , some "background_completion:"
        )
      , ("restart_native_background_terminal_parent_lost_on_restart"
        , some "interrupted_on_restart"
        , some "background_completion"
        , some "background_completion:"
        ) ] := by
  native_decide

theorem restartDispositionCases_leave_running_rows_carry_no_terminal :
    ∀ witness ∈ restartDispositionCases,
      witness.disposition ≠ "terminalize" →
        witness.cause = none ∧ witness.terminalState = none := by
  native_decide

theorem restartDispositionCases_all_recover_all :
    ∀ witness ∈ restartDispositionCases,
      witness.rustFunction = "ToolCallLifecycle::recover_all" := by
  native_decide

theorem recoverySweepCases_registered_sweeps :
    ∀ witness : RecoverySweepCase,
      witness ∈ recoverySweepCases →
      (witness.sweepId, witness.collection) ∈ registeredRecoverySweepContracts := by
  native_decide

theorem recoverySweepCases_decrease_or_defer :
    ∀ witness,
      witness ∈ recoverySweepCases →
      (witness.measureBefore > witness.measureAfter ∧ witness.measureAfter = 0) ∨
        (witness.measureBefore = 0 ∧ witness.measureAfter = 0 ∧
          witness.preState = witness.terminalState) := by
  native_decide

end Recovery
