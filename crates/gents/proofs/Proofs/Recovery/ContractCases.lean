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
    (deadlineExpired unclaimedExpired parentLive parentInterrupted
      parentTerminal executionRegistered : Bool)
    (process : ManagedExec.StopOutcome := .stopped)
    (ownerTaskDeleted : Bool := false) : RecoverySweepCase :=
  let row : OrphanedBackgroundToolRow :=
    { call := r6NativeToolFixture
    , deadlineExpired := deadlineExpired
    , unclaimedExpired := unclaimedExpired
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
        | .childCompleted => "child_completed"
        | .childFailed => "child_failed"
        | .childDead => "child_dead"
        | .childInterrupted => "child_interrupted"
        | .childSuperseded => "child_superseded"
        | .unclaimedCrossPrincipalSpawn => "unclaimed_spawn_timeout"
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
    unclaimedExpired := some row.unclaimedExpired
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
      false false true false false false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_expired_missing_parent_deferred"
      true false false false false false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_unclaimed_missing_parent_deferred"
      false true false false false false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_expired_terminal_parent_to_timed_out"
      true false false false true false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_unclaimed_to_failed"
      false true true false false false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_terminal_parent_to_cancelled"
      false false false false true false
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_unowned_process_to_failed"
      false false true false false false (process := .notOwned)
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_exited_process_to_failed"
      false false false false true false (process := .alreadyExited)
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_still_running_deferred"
      false false true false false false (process := .stillRunning)
  , orphanedBackgroundRecoveryCase
      "registered_background_tool_left_to_worker_deferred"
      false false true false false true
  , orphanedBackgroundRecoveryCase
      "registered_background_tool_task_deleted_to_cancelled"
      false false true false false true (ownerTaskDeleted := true)
  , orphanedBackgroundRecoveryCase
      "orphaned_background_tool_interrupted_parent_to_cancelled"
      false false false true false false
  , recoveryCase
      backgroundCompletionSideEffectSweep
      "terminal_background_tool_missing_completion_side_effects_to_converged"
      "failed"
      "failed"
      "r6-cross-turn-background-process-durability"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_unclaimed_cross_principal_spawn_to_failed"
      "running"
      "failed"
      "r5-cross-principal-subagents-design"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_child_completed_to_completed"
      "running"
      "completed"
      "deadline-plumbing-audit-2026-05-12-subagent-bridge-terminal-lifetime"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_child_failed_to_failed"
      "running"
      "failed"
      "deadline-plumbing-audit-2026-05-12-subagent-bridge-terminal-lifetime"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_child_interrupted_to_cancelled"
      "running"
      "cancelled"
      "deadline-plumbing-audit-2026-05-12-subagent-bridge-terminal-lifetime"
  , recoveryCase
      detachedBridgeRecoverySweep
      "detached_bridge_child_completed_to_completed"
      "running"
      "completed"
      "deadline-plumbing-audit-2026-05-12-subagent-bridge-terminal-lifetime"
  , recoveryCase
      detachedBridgeRecoverySweep
      "detached_bridge_child_failed_to_failed"
      "running"
      "failed"
      "deadline-plumbing-audit-2026-05-12-subagent-bridge-terminal-lifetime"
  , recoveryCase
      detachedBridgeRecoverySweep
      "detached_bridge_child_interrupted_to_cancelled"
      "running"
      "cancelled"
      "deadline-plumbing-audit-2026-05-12-subagent-bridge-terminal-lifetime"
  , recoveryCase
      detachedBridgeRecoverySweep
      "detached_bridge_deadline_exceeded_to_timed_out"
      "running"
      "timedOut"
      "deadline-plumbing-audit-2026-05-12-subagent-bridge-terminal-lifetime"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_child_dead_to_failed"
      "running"
      "failed"
      "gents-465-subagent-liveness"
  , recoveryCase
      expiredSubagentChildSweep
      "expired_processing_child_to_dead"
      "processing"
      "dead"
      "gents-465-subagent-liveness"
  , recoveryCase
      expiredSubagentChildSweep
      "expired_claimed_child_to_dead"
      "claimed"
      "dead"
      "gents-465-subagent-liveness"
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
    (awaitMode : Subagent.AwaitMode)
    (cancelPolicy : Subagent.CancelPolicy)
    (childLinked : Bool)
    (parent : ParentObservation)
    (theoremName : String)
    (deadlineExpired : Bool := false)
    (unclaimedExpired : Bool := false)
    (process : ManagedExec.StopOutcome := .stopped)
    (childObserved : Bool := false) : RestartDispositionCase :=
  let row : RestartRow :=
    { awaitMode := awaitMode
    , cancelPolicy := cancelPolicy
    , childLinked := childLinked
    , parent := parent
    , deadlineExpired := deadlineExpired
    , unclaimedExpired := unclaimedExpired
    , process := process
    , childObserved := childObserved
    }
  let disposition := restartDisposition row
  { name := name
  , rustFunction := "ToolCallLifecycle::recover_all"
  , awaitMode := awaitMode.toDefraDB
  , cancelPolicy := cancelPolicy.toDefraDB
  , childLinked := childLinked
  , parentObservation := parent.toContract
  , deadlineExpired := deadlineExpired
  , unclaimedExpired := unclaimedExpired
  , processOutcome := process.toContract
  , childObserved := childObserved
  , bridgeCancelIntent := row.spawnFence.map (·.cancelIntent)
  , bridgeAckPending := row.spawnFence.map (·.ackPending)
  , disposition := disposition.toContract
  , cause := disposition.causeContract
  , terminalState := disposition.terminalStateContract
  , postAwaitMode := disposition.postAwaitModeContract
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
      .background .cascade false .live
      "Recovery.native_background_tool_resolvable_parent_interrupted_on_restart"
  , restartDispositionCase
      "restart_native_background_unowned_process_lost"
      .background .cascade false .live
      "Recovery.native_background_unstopped_process_settles_lost"
      (process := .notOwned)
  , restartDispositionCase
      "restart_native_background_exited_process_lost"
      .background .cascade false .otherTerminal
      "Recovery.native_background_unstopped_process_settles_lost"
      (process := .alreadyExited)
  , restartDispositionCase
      "restart_native_background_still_running_left_running"
      .background .cascade false .live
      "Recovery.native_background_still_running_left_running"
      (process := .stillRunning)
  , restartDispositionCase
      "restart_background_subagent_live_parent_left_running"
      .background .cascade true .live
      "Recovery.background_subagent_bridge_live_parent_left_running"
  , restartDispositionCase
      "restart_detached_bridge_interrupted_parent_retained"
      .background .detach true .interrupted
      "Recovery.child_linked_terminal_parent_retained_in_background"
  , restartDispositionCase
      "restart_cascade_bridge_interrupted_parent_retained"
      .background .cascade true .interrupted
      "Recovery.child_linked_terminal_parent_retained_in_background"
  , restartDispositionCase
      "restart_awaited_bridge_interrupted_parent_backgrounded"
      .foreground .cascade true .interrupted
      "Recovery.child_linked_terminal_parent_retained_in_background"
  , restartDispositionCase
      "restart_awaited_bridge_failed_parent_backgrounded"
      .foreground .cascade true .otherTerminal
      "Recovery.child_linked_terminal_parent_retained_in_background"
  , restartDispositionCase
      "restart_background_bridge_failed_parent_retained"
      .background .cascade true .otherTerminal
      "Recovery.child_linked_terminal_parent_retained_in_background"
  , restartDispositionCase
      "restart_clean_complete_child_linked_retained"
      .background .cascade true .cleanlyCompleted
      "Recovery.child_linked_terminal_parent_retained_in_background"
  , restartDispositionCase
      "restart_native_background_deadline_expired_times_out"
      .background .cascade false .live
      "Recovery.deadline_precedes_restart_interrupt"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_interrupted_parent_lost_on_restart"
      .background .cascade false .interrupted
      "Recovery.native_background_tool_interrupted_on_restart"
  , restartDispositionCase
      "restart_native_background_terminal_parent_lost_on_restart"
      .background .cascade false .otherTerminal
      "Recovery.native_background_tool_interrupted_on_restart"
  , restartDispositionCase
      "restart_foreground_interrupted_parent_cancelled"
      .foreground .cascade false .interrupted
      "Recovery.foreground_unlinked_interrupted_parent_terminalizes"
  , restartDispositionCase
      "restart_foreground_live_parent_left_running"
      .foreground .cascade false .live
      "Recovery.leave_running_iff_preserved_shapes"
  , restartDispositionCase
      "restart_subagent_missing_parent_left_running"
      .background .cascade true .missing
      "Recovery.missing_parent_never_terminalizes"
  , restartDispositionCase
      "restart_native_background_expired_missing_parent_deferred"
      .background .cascade false .missing
      "Recovery.missing_parent_never_terminalizes"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_unclaimed_missing_parent_deferred"
      .background .cascade true .missing
      "Recovery.missing_parent_never_terminalizes"
      (unclaimedExpired := true)
  , -- Unclaimed cross-principal spawn expiry outranks every leave-running
    -- exemption: an unclaimed bridge under a live parent still fails.
    restartDispositionCase
      "restart_unclaimed_spawn_expired_fails"
      .background .cascade true .live
      "Recovery.unclaimed_precedes_leave_running_exemptions"
      (unclaimedExpired := true)
  , restartDispositionCase
      "restart_unclaimed_observed_child_links"
      .background .cascade true .live
      "Recovery.unclaimed_observed_child_links"
      (unclaimedExpired := true) (childObserved := true)
  , restartDispositionCase
      "restart_deadline_unobserved_child_fenced"
      .background .cascade true .cleanlyCompleted
      "Recovery.restart_expiry_fences_unobserved_child"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_both_expired_unobserved_child_fenced"
      .background .cascade true .live
      "Recovery.restart_expiry_fences_unobserved_child"
      (deadlineExpired := true) (unclaimedExpired := true)
  ]

/-- The witness family covers every disposition, including expired and
    unclaimed rows whose missing physical parent defers classification. -/
theorem restartDispositionCases_cover_every_disposition :
    (restartDispositionCases.filter
        (fun witness => witness.disposition = "leave_running")).length = 6 ∧
      (restartDispositionCases.filter
        (fun witness => witness.disposition = "retain_in_background")).length = 6 ∧
      (restartDispositionCases.filter
        (fun witness => witness.disposition = "terminalize")).length = 10 ∧
      (restartDispositionCases.filter
        (fun witness => witness.disposition = "link")).length = 1 := by
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
