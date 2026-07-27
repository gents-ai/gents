import Proofs.Recovery.Sweeps
import Proofs.Conformance.ContractCases

/-!
# Recovery Sweep Conformance Cases

Finite witness rows emitted to Rust so every persisted startup recovery sweep
has an explicit consumer or follow-up obligation.
-/

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

/-! ## Outcome witnesses (#693)

These rows fence the two defects directly. `expectedRecovered` counts
SUCCESSES: a duplicate group whose write the store refuses reports zero, never
the number of docs it attempted. `targetSelector` pins the addressing mode that
makes the write possible at all. -/

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
  [ -- The healthy single-doc session: one session in, one recovery reported.
    outcomeCase
      "conversation_single_doc_recovers_and_counts_one"
      1 false true 1 0 0
      "Recovery.Step.all_succeeded_reports_all"
    -- #693 defect 1: two docs share a session_id. Addressed by _docID the write
    -- lands, the whole group is terminalized, and the SESSION counts once.
  , outcomeCase
      "conversation_duplicate_group_recovers_canonical_and_counts_one"
      2 true true 1 0 0
      "Recovery.duplicate_group_recovers"
    -- #693 defect 2: the store refuses the write (the pre-fix session_id-filter
    -- upsert on a duplicate store). The sweep MUST report zero recoveries —
    -- never `rows.len()` — and leave the docs stale for the next pass.
  , outcomeCase
      "conversation_failed_write_reports_zero_recovered"
      2 true false 0 1 2
      "Recovery.Step.all_failed_reports_zero"
    -- Idempotence: a second pass over an already-recovered store finds nothing.
  , outcomeCase
      "conversation_second_pass_recovers_nothing"
      2 true true 0 0 0
      "Recovery.conversation_recover_idempotent"
  ]

theorem recoveryOutcomeCases_address_docs_by_docId :
    ∀ witness ∈ recoveryOutcomeCases, witness.targetSelector = "_docID" := by
  native_decide

/-- The reported count never exceeds the number of sessions swept, and a failed
    write reports zero recoveries — #693 defect 2, as a checkable row. -/
theorem recoveryOutcomeCases_count_only_successes :
    ∀ witness ∈ recoveryOutcomeCases,
      (witness.writeSucceeds = false → witness.expectedRecovered = 0) ∧
      witness.expectedRecovered ≤ 1 := by
  native_decide

/-- A failed write leaves the group stale: nothing converged, so it must be
    retried rather than reported as done. -/
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
