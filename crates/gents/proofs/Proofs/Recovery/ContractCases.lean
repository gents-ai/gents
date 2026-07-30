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

def recoverySweepCases : List RecoverySweepCase :=
  [ recoveryCase
      requestRecoverySweep
      "request_claimed_recovery_to_failed"
      "claimed"
      "failed"
      "formal-coverage-audit-2026-05-13-gap-6"
  , recoveryCase
      requestRecoverySweep
      "request_processing_terminal_response_recovery_to_completed"
      "processing"
      "completed"
      "gents-664-durable-terminal-repair"
  , recoveryCase
      requestRecoverySweep
      "request_processing_recovery_to_failed"
      "processing"
      "failed"
      "formal-coverage-audit-2026-05-13-gap-6"
  , recoveryCase
      requestRecoverySweep
      "request_processing_interrupted_response_recovery_to_interrupted"
      "processing"
      "interrupted"
      "gents-664-durable-terminal-repair"
  , recoveryCase
      responseRecoverySweep
      "response_streaming_recovery_to_error"
      "streaming"
      "error"
      "deadline-plumbing-audit-2026-05-12-streaming-response-lifetime"
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
      "live_running_composite_parent_interrupted_to_cancelled"
      "running"
      "cancelled"
      "gents-837-terminalize-interrupted-composites"
  , recoveryCase
      terminalParentOwnedToolSweep
      "live_running_tool_parent_terminal_to_failed"
      "running"
      "failed"
      "gents-837-terminalize-interrupted-composites"
  ,
    recoveryCase
      terminalParentOwnedToolSweep
      "live_detached_bridge_parent_failed_to_failed"
      "running"
      "failed"
      "gents-837-terminalize-interrupted-composites"
  , recoveryCase
      toolCallRecoverySweep
      "tool_backgrounded_running_live_parent_to_cancelled"
      "running"
      "cancelled"
      "r6-TerminalizeBackgroundedAsInterrupted"
  , recoveryCase
      toolCallRecoverySweep
      "tool_running_unclaimed_cross_deployment_spawn_to_failed"
      "running"
      "failed"
      "r5-cross-deployment-subagents-design"
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
      "detached_bridge_terminal_parent_to_failed"
      "running"
      "failed"
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
      queuedDescendantSweep
      "queued_descendant_terminal_parent_to_interrupted"
      "pending"
      "interrupted"
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
  , recoveryCase
      conversationRecoverySweep
      "conversation_processing_completed_parent_to_completed"
      "processing"
      "completed"
      "gents-693-conversation-recovery"
  , recoveryCase
      conversationRecoverySweep
      "conversation_processing_unfinished_parent_to_active"
      "processing"
      "active"
      "gents-693-conversation-recovery"
  , recoveryCase
      conversationRecoverySweep
      "conversation_error_unfinished_parent_to_active"
      "error"
      "active"
      "gents-693-conversation-recovery"
  ]

def outcomeCase
    (name : String)
    (docCount : Nat)
    (duplicated writeSucceeds : Bool)
    (expectedRecovered expectedFailed measureAfter : Nat)
    (theoremName : String) : RecoveryOutcomeCase :=
  { name := name
  , sweepId := conversationRecoverySweep.sweepId
  , collection := conversationRecoverySweep.collection.toContract
  , rustFunction := conversationRecoverySweep.rustFunction
  , docCount := docCount
  , duplicated := duplicated
  , writeSucceeds := writeSucceeds
  , expectedRecovered := expectedRecovered
  , expectedFailed := expectedFailed
  , measureAfter := measureAfter
  , targetSelector := "_docID"
  , theoremName := theoremName
  }

def recoveryOutcomeCases : List RecoveryOutcomeCase :=
  [
    outcomeCase
      "conversation_single_doc_recovers_and_counts_one"
      1 false true 1 0 0
      "Recovery.Step.all_succeeded_reports_all"
  , outcomeCase
      "conversation_duplicate_group_recovers_canonical_and_counts_one"
      2 true true 1 0 0
      "Recovery.duplicate_group_recovers"
  , outcomeCase
      "conversation_failed_write_reports_zero_recovered"
      2 true false 0 1 2
      "Recovery.Step.all_failed_reports_zero"
  , outcomeCase
      "conversation_second_pass_recovers_nothing"
      2 true true 0 0 0
      "Recovery.conversation_recover_idempotent"
  ]

theorem recoveryOutcomeCases_address_docs_by_docId :
    ∀ witness ∈ recoveryOutcomeCases, witness.targetSelector = "_docID" := by
  native_decide

theorem recoveryOutcomeCases_count_only_successes :
    ∀ witness ∈ recoveryOutcomeCases,
      (witness.writeSucceeds = false → witness.expectedRecovered = 0) ∧
      witness.expectedRecovered ≤ 1 := by
  native_decide

theorem recoveryOutcomeCases_failed_write_leaves_rows_stale :
    ∀ witness ∈ recoveryOutcomeCases,
      witness.writeSucceeds = false → witness.measureAfter > 0 := by
  native_decide

def recoveryEquivalenceTheorem (sweepId : String) : String :=
  if sweepId = requestRecoverySweep.sweepId then
    "Recovery.requestRecover_matches_uninterrupted"
  else if sweepId = responseRecoverySweep.sweepId then
    "Recovery.responseRecover_matches_uninterrupted"
  else if sweepId = toolCallRecoverySweep.sweepId then
    "Recovery.toolCallRecover_matches_uninterrupted"
  else if sweepId = terminalParentOwnedToolSweep.sweepId then
    "Recovery.terminalParentToolRecover_matches_uninterrupted"
  else if sweepId = detachedBridgeRecoverySweep.sweepId then
    "Recovery.detachedBridgeRecover_matches_uninterrupted"
  else if sweepId = inferenceCallRecoverySweep.sweepId then
    "Recovery.inferenceCallRecover_matches_uninterrupted"
  else if sweepId = expiredSubagentChildSweep.sweepId then
    "Recovery.expiredChildRecover_matches_uninterrupted"
  else if sweepId = queuedDescendantSweep.sweepId then
    "Recovery.queuedDescendantRecover_matches_uninterrupted"
  else if sweepId = conversationRecoverySweep.sweepId then
    "Recovery.conversation_recover_matches_uninterrupted"
  else
    "unregistered_recovery_equivalence"

def recoveryEquivalenceCase
    (witness : RecoverySweepCase) : RecoveryEquivalenceCase :=
  { name := witness.name ++ "_same_as_uninterrupted"
  , sourceSweepCase := witness.name
  , sweepId := witness.sweepId
  , collection := witness.collection
  , rustFunction := witness.rustFunction
  , cadence := witness.cadence
  , preState := witness.preState
  , recoveredState := witness.terminalState
  , uninterruptedState := witness.terminalState
  , equivalent := true
  , reexecutes := false
  , canHang := false
  , theoremName := recoveryEquivalenceTheorem witness.sweepId
  , aggregateTheoremName :=
      "Recovery.RecoveryEquivalence.finite_stale_rows_converge_to_uninterrupted"
  }

def recoveryEquivalenceCases : List RecoveryEquivalenceCase :=
  recoverySweepCases.map recoveryEquivalenceCase

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
    (unclaimedExpired : Bool := false) : RestartDispositionCase :=
  let row : RestartRow :=
    { awaitMode := awaitMode
    , cancelPolicy := cancelPolicy
    , childLinked := childLinked
    , parent := parent
    , deadlineExpired := deadlineExpired
    , unclaimedExpired := unclaimedExpired
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
  , disposition := disposition.toContract
  , cause := disposition.causeContract
  , terminalState := disposition.terminalStateContract
  , notificationReason :=
      disposition.notification.map RestartNotificationObligation.notificationReason
  , queueSource :=
      disposition.notification.map RestartNotificationObligation.queueSource
  , queueKeyPrefix :=
      disposition.notification.map RestartNotificationObligation.queueKeyPrefix
  , theoremName := theoremName
  }

def restartDispositionCases : List RestartDispositionCase :=
  [ restartDispositionCase
      "restart_native_background_live_parent_interrupted"
      .background .cascade false .live
      "Recovery.native_background_tool_live_parent_interrupted_on_restart"
  , restartDispositionCase
      "restart_background_subagent_live_parent_left_running"
      .background .cascade true .live
      "Recovery.background_subagent_bridge_live_parent_left_running"
  , restartDispositionCase
      "restart_detached_bridge_interrupted_parent_left_running"
      .background .detach true .interrupted
      "Recovery.detached_bridge_interrupted_parent_left_running"
  , restartDispositionCase
      "restart_clean_complete_child_linked_left_running"
      .background .cascade true .cleanlyCompleted
      "Recovery.clean_completion_child_linked_left_running"
  , restartDispositionCase
      "restart_native_background_deadline_expired_times_out"
      .background .cascade false .live
      "Recovery.deadline_precedes_restart_interrupt"
      (deadlineExpired := true)
  , restartDispositionCase
      "restart_native_background_interrupted_parent_cancelled"
      .background .cascade false .interrupted
      "Recovery.restart_interrupt_iff_native_background_live_parent"
  , restartDispositionCase
      "restart_native_background_terminal_parent_failed"
      .background .cascade false .otherTerminal
      "Recovery.restart_interrupt_iff_native_background_live_parent"
  , restartDispositionCase
      "restart_foreground_live_parent_left_running"
      .foreground .cascade false .live
      "Recovery.leave_running_iff_preserved_shapes"
  , restartDispositionCase
      "restart_subagent_missing_parent_left_running"
      .background .cascade true .missing
      "Recovery.leave_running_iff_preserved_shapes"
  , -- Unclaimed cross-deployment spawn expiry outranks every leave-running
    -- exemption: an unclaimed bridge under a live parent still fails.
    restartDispositionCase
      "restart_unclaimed_spawn_expired_fails"
      .background .cascade true .live
      "Recovery.unclaimed_precedes_leave_running_exemptions"
      (unclaimedExpired := true)
  ]

/-- The witness family covers both dispositions and pins the expected split:
    five leave-running rows (background subagent + live parent, detached +
    interrupted parent, clean-complete + child-linked, foreground + live
    parent, missing parent), five terminalize rows. -/
theorem restartDispositionCases_cover_both_dispositions :
    (restartDispositionCases.filter
        (fun witness => witness.disposition = "leave_running")).length = 5 ∧
      (restartDispositionCases.filter
        (fun witness => witness.disposition = "terminalize")).length = 5 := by
  native_decide

/-- Exactly one row owes the restart notification, and it is the native
    background live-parent interrupt with the pinned reason and queue
    vocabulary. -/
theorem restartDispositionCases_notification_unique :
    (restartDispositionCases.filter
        (fun witness => witness.notificationReason.isSome)).map
        (fun witness =>
          (witness.name, witness.notificationReason, witness.queueSource,
            witness.queueKeyPrefix)) =
      [ ("restart_native_background_live_parent_interrupted"
        , some "interrupted_on_restart"
        , some "background_completion"
        , some "background_completion:"
        ) ] := by
  native_decide

/-- Leave-running rows carry no cause and no terminal state — the row is
    preserved verbatim. -/
theorem restartDispositionCases_leave_running_rows_carry_no_terminal :
    ∀ witness ∈ restartDispositionCases,
      witness.disposition = "leave_running" →
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

theorem recoverySweepCases_decrease_to_zero :
    ∀ witness,
      witness ∈ recoverySweepCases →
      witness.measureBefore > witness.measureAfter ∧ witness.measureAfter = 0 := by
  native_decide

theorem recoveryEquivalenceCases_cover_recoverySweepCases :
    recoveryEquivalenceCases.length = recoverySweepCases.length := by
  native_decide

theorem recoveryEquivalenceCases_same_as_uninterrupted :
    ∀ witness,
      witness ∈ recoveryEquivalenceCases →
      witness.recoveredState = witness.uninterruptedState ∧
      witness.equivalent = true ∧
      witness.reexecutes = false ∧
      witness.canHang = false ∧
      witness.theoremName ≠ "unregistered_recovery_equivalence" := by
  native_decide

end Recovery
