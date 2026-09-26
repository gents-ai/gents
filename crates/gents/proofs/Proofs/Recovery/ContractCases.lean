import Proofs.Recovery.Sweeps
import Proofs.Conformance.ContractCases

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
    (ownerTaskDeleted : Bool := false) : RecoverySweepCase :=
  let row : OrphanedBackgroundToolRow :=
    { call := r6NativeToolFixture
    , deadlineExpired := deadlineExpired
    , parentLive := parentLive
    , parentInterrupted := parentInterrupted
    , parentTerminal := parentTerminal
    , executionRegistered := executionRegistered
    , process := process
    , ownerTaskDeleted := ownerTaskDeleted
    }
  let recovered := orphanedBackgroundToolRecover row
  let cause := orphanedBackgroundToolCause row
  let notificationReason :=
    if row.parentLive || row.parentInterrupted || row.parentTerminal then
      cause.map fun recoveryCause =>
        match recoveryCause with
        | .deadlineExceeded => "deadline_exceeded"
        | .parentInterrupted => "parent_interrupted"
        | .parentTerminal => "parent_terminal"
        | .terminalizeBackgroundedAsInterrupted => "interrupted_on_restart"
        | .processLost => "process_lost"
        | .taskDeleted => "task_deleted"
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
    ownerTaskDeleted := some row.ownerTaskDeleted
    recoveryCause := cause.map ToolRecoveryCause.toContract
    notificationReason := notificationReason
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
      "orphaned_background_tool_interrupted_parent_to_cancelled"
      false false true false false
  , recoveryCase
      backgroundCompletionSideEffectSweep
      "terminal_background_tool_missing_completion_side_effects_to_converged"
      "failed"
      "failed"
      "r6-cross-turn-background-process-durability"
  , recoveryCase
      sessionMessageRecoverySweep
      "session_message_request_completed_to_completed"
      "running"
      "completed"
      "session-message-stack-observer-arm"
  , recoveryCase
      sessionMessageRecoverySweep
      "session_message_request_failed_to_failed"
      "running"
      "failed"
      "session-message-stack-observer-arm"
  , recoveryCase
      sessionMessageRecoverySweep
      "session_message_request_interrupted_to_cancelled"
      "running"
      "cancelled"
      "session-message-stack-observer-arm"
  , recoveryCase
      sessionMessageRecoverySweep
      "session_message_request_dead_to_failed"
      "running"
      "failed"
      "session-message-stack-observer-arm"
  , recoveryCase
      sessionMessageRecoverySweep
      "session_message_deadline_exceeded_to_timed_out"
      "running"
      "timedOut"
      "session-message-stack-observer-arm"
  , recoveryCase
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
  ]

/-! ## Restart disposition witnesses (#937)

Finite rows for the startup classifier in
`recover_stuck_running_tool_calls`. Unlike `recoverySweepCases`, the
`disposition`/`cause`/`terminalState`/notification fields are **computed from
`Recovery.restartDisposition`**, so these rows cannot drift from the model:
changing a classifier branch changes the emitted JSON and fails the Rust
consumer. The leave-running rows are the previously inexpressible outcomes —
background subagent bridges and detached/clean-complete bridges that startup
recovery must preserve. -/

def restartDispositionCase
    (name : String)
    (awaitMode : ToolExecution.AwaitMode)
    (sessionMessage : Bool)
    (parent : ParentObservation)
    (theoremName : String)
    (deadlineExpired : Bool := false)
    (process : ManagedExec.StopOutcome := .stopped) : RestartDispositionCase :=
  let row : RestartRow :=
    { awaitMode := awaitMode
    , sessionMessage := sessionMessage
    , parent := parent
    , deadlineExpired := deadlineExpired
    , process := process
    }
  let disposition := restartDisposition row
  { name := name
  , rustFunction := "ToolCallLifecycle::recover_all"
  , awaitMode := awaitMode.toDefraDB
  , sessionMessage := sessionMessage
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
      .background false .live
      "Recovery.native_background_tool_live_parent_interrupted_on_restart"
  , restartDispositionCase
      "restart_native_background_unowned_process_lost"
      .background false .live
      "Recovery.native_background_unstopped_process_settles_lost"
      (process := .notOwned)
  , restartDispositionCase
      "restart_native_background_exited_process_lost"
      .background false .otherTerminal
      "Recovery.native_background_unstopped_process_settles_lost"
      (process := .alreadyExited)
  , restartDispositionCase
      "restart_native_background_still_running_left_running"
      .background false .live
      "Recovery.native_background_still_running_left_running"
      (process := .stillRunning)
  , restartDispositionCase
      "restart_session_message_live_parent_left_running"
      .background true .live
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_interrupted_parent_left_running"
      .background true .interrupted
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_failed_parent_left_running"
      .background true .otherTerminal
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_clean_complete_parent_left_running"
      .background true .cleanlyCompleted
      "Recovery.session_message_row_left_running"
  , restartDispositionCase
      "restart_session_message_deadline_expired_times_out"
      .background true .live
      "Recovery.session_message_row_terminalizes_only_on_expiry"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_deadline_expired_times_out"
      .background false .live
      "Recovery.deadline_precedes_restart_interrupt"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_interrupted_parent_lost_on_restart"
      .background false .interrupted
      "Recovery.native_background_tool_interrupted_on_restart"
  , restartDispositionCase
      "restart_native_background_terminal_parent_lost_on_restart"
      .background false .otherTerminal
      "Recovery.native_background_tool_interrupted_on_restart"
  , restartDispositionCase
      "restart_foreground_interrupted_parent_cancelled"
      .foreground false .interrupted
      "Recovery.leave_running_iff_preserved_shapes"
  , restartDispositionCase
      "restart_foreground_live_parent_left_running"
      .foreground false .live
      "Recovery.leave_running_iff_preserved_shapes"
  , restartDispositionCase
      "restart_session_message_missing_parent_left_running"
      .background true .missing
      "Recovery.missing_parent_never_terminalizes"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_expired_missing_parent_deferred"
      .background false .missing
      "Recovery.missing_parent_never_terminalizes"
      (deadlineExpired := true)
  ]

/-- The witness family covers every disposition, including expired rows whose
    missing physical parent defers classification. -/
theorem restartDispositionCases_cover_every_disposition :
    (restartDispositionCases.filter
        (fun witness => witness.disposition = "leave_running")).length = 9 ∧
      (restartDispositionCases.filter
        (fun witness => witness.disposition = "terminalize")).length = 7 := by
  native_decide

/-- Every terminal native background witness with a resolvable parent owes a
    completion notification and coalesced wake. -/
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

/-- Leave-running rows carry no cause and no terminal state — the row is
    preserved verbatim. -/
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
