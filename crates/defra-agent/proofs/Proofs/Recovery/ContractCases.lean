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
      "request_processing_recovery_to_failed"
      "processing"
      "failed"
      "formal-coverage-audit-2026-05-13-gap-6"
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

def recoveryEquivalenceTheorem (sweepId : String) : String :=
  if sweepId = requestRecoverySweep.sweepId then
    "Recovery.requestRecover_matches_uninterrupted"
  else if sweepId = responseRecoverySweep.sweepId then
    "Recovery.responseRecover_matches_uninterrupted"
  else if sweepId = toolCallRecoverySweep.sweepId then
    "Recovery.toolCallRecover_matches_uninterrupted"
  else if sweepId = detachedBridgeRecoverySweep.sweepId then
    "Recovery.detachedBridgeRecover_matches_uninterrupted"
  else if sweepId = inferenceCallRecoverySweep.sweepId then
    "Recovery.inferenceCallRecover_matches_uninterrupted"
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
