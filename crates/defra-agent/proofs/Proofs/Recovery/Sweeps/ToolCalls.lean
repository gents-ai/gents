import Proofs.Recovery.Sweeps.RequestResponse
import Proofs.ToolExecution

/-! Regular tool-call startup-recovery sweep contracts and shared predicates. -/

namespace Recovery

open ToolExecution

/-! ## Tool-call recovery -/

def isDetachedBridgeCall (call : ToolCallContext) : Prop :=
  call.childRequestId.isSome ∧ call.cancelPolicy = .detach

instance (call : ToolCallContext) : Decidable (isDetachedBridgeCall call) := by
  unfold isDetachedBridgeCall
  infer_instance

inductive ToolRecoveryCause where
  | deadlineExceeded
  | parentInterrupted
  | parentTerminal
  | terminalizeBackgroundedAsInterrupted
  | childCompleted
  | childFailed
  | childDead
  | childInterrupted
  | childSuperseded
  | unclaimedCrossDeploymentSpawn
  deriving DecidableEq, Repr

namespace ToolRecoveryCause

def toContract : ToolRecoveryCause → String
  | .deadlineExceeded => "deadlineExceeded"
  | .parentInterrupted => "parentInterrupted"
  | .parentTerminal => "parentTerminal"
  | .terminalizeBackgroundedAsInterrupted => "TerminalizeBackgroundedAsInterrupted"
  | .childCompleted => "childCompleted"
  | .childFailed => "childFailed"
  | .childDead => "childDead"
  | .childInterrupted => "childInterrupted"
  | .childSuperseded => "childSuperseded"
  | .unclaimedCrossDeploymentSpawn => "unclaimedCrossDeploymentSpawn"

def terminalState : ToolRecoveryCause → ToolCallState
  | .deadlineExceeded => .timedOut
  | .parentInterrupted => .cancelled
  | .parentTerminal => .failed
  | .terminalizeBackgroundedAsInterrupted => .cancelled
  | .childCompleted => .completed
  | .childFailed => .failed
  | .childDead => .failed
  | .childInterrupted => .cancelled
  | .childSuperseded => .failed
  | .unclaimedCrossDeploymentSpawn => .failed

theorem terminalState_terminal (cause : ToolRecoveryCause) :
    isTerminal cause.terminalState := by
  cases cause <;>
    simp [terminalState, HasTerminal.isTerminal, ToolCallState.instHasTerminal]

end ToolRecoveryCause

def isBackgroundedRunningWithLiveParent
    (row : ToolCallContext) (parent : RequestContext) : Prop :=
  row.awaitMode = .background ∧
  row.state = .running ∧
  ¬ isTerminal parent.state

instance (row : ToolCallContext) (parent : RequestContext) :
    Decidable (isBackgroundedRunningWithLiveParent row parent) := by
  unfold isBackgroundedRunningWithLiveParent
  infer_instance

structure ToolCallRecoveryRow where
  call : ToolCallContext
  cause : ToolRecoveryCause
  deriving Repr

def toolCallRecoveryStale (row : ToolCallRecoveryRow) : Prop :=
  row.call.state = .running ∧ ¬ isDetachedBridgeCall row.call

instance (row : ToolCallRecoveryRow) : Decidable (toolCallRecoveryStale row) := by
  unfold toolCallRecoveryStale
  infer_instance

def toolCallRecover (row : ToolCallRecoveryRow) : ToolCallRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def toolCallUninterruptedTerminalize (row : ToolCallRecoveryRow) : ToolCallRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def toolCallRecoveryMeasure (row : ToolCallRecoveryRow) : Nat :=
  if toolCallRecoveryStale row then 1 else 0

theorem toolCallRecover_matches_uninterrupted :
    ∀ row, toolCallRecoveryStale row →
      toolCallRecover row = toolCallUninterruptedTerminalize row := by
  intro row _h_stale
  simp [toolCallRecover, toolCallUninterruptedTerminalize]

theorem toolCallRecovery_stale_positive :
    ∀ row, toolCallRecoveryStale row → toolCallRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [toolCallRecoveryMeasure, h_stale]

theorem toolCallRecover_terminal :
    ∀ row, toolCallRecoveryStale row → isTerminal (toolCallRecover row).call.state := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [toolCallRecover, ToolRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]

theorem toolCallRecover_zero :
    ∀ row, toolCallRecoveryStale row → toolCallRecoveryMeasure (toolCallRecover row) = 0 := by
  intro row _h_stale
  have h_terminal_not_running : row.cause.terminalState ≠ .running := by
    cases row.cause <;> simp [ToolRecoveryCause.terminalState]
  have h_not : ¬ toolCallRecoveryStale (toolCallRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_running, _h_not_detached⟩
    simp [toolCallRecover] at h_running
    exact h_terminal_not_running h_running
  simp [toolCallRecoveryMeasure, h_not]

def toolCallRecoverySweep : RecoverySweep :=
  { Row := ToolCallRecoveryRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_recover_all_running_calls"
  , rustFunction := "ToolCallLifecycle::recover_all"
  , cadence := .startup
  , implementationStatus := .implemented
  , stale := toolCallRecoveryStale
  , recover := toolCallRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := toolCallRecoveryMeasure
  , h_stale_positive := toolCallRecovery_stale_positive
  , h_recover_terminal := toolCallRecover_terminal
  , h_recover_zero := toolCallRecover_zero
  }

def toolCallRecoveryEquivalence : RecoveryEquivalence toolCallRecoverySweep :=
  { uninterrupted := toolCallUninterruptedTerminalize
  , h_recover_eq_uninterrupted := toolCallRecover_matches_uninterrupted
  }

end Recovery
