import Proofs.Runtime.Recovery.Sweeps
import Proofs.Conformance.ContractCases
import Proofs.Conformance.PluginJob

namespace Recovery

open Conformance.ContractCases

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

end Recovery
