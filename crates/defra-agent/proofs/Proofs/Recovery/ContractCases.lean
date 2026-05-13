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

end Recovery
