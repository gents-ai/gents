import Proofs.Recovery.Sweeps.ToolCalls

namespace Recovery

open ToolExecution

inductive DetachedBridgeRecoveryCause where
  | deadlineExceeded
  | terminalParent
  | childCompleted
  | childFailed
  | childDead
  | childInterrupted
  | childSuperseded
  deriving DecidableEq, Repr

namespace DetachedBridgeRecoveryCause

def toContract : DetachedBridgeRecoveryCause → String
  | .deadlineExceeded => "deadlineExceeded"
  | .terminalParent => "terminalParent"
  | .childCompleted => "childCompleted"
  | .childFailed => "childFailed"
  | .childDead => "childDead"
  | .childInterrupted => "childInterrupted"
  | .childSuperseded => "childSuperseded"

def terminalState : DetachedBridgeRecoveryCause → ToolCallState
  | .deadlineExceeded => .timedOut
  | .terminalParent => .failed
  | .childCompleted => .completed
  | .childFailed => .failed
  | .childDead => .failed
  | .childInterrupted => .cancelled
  | .childSuperseded => .failed

theorem terminalState_terminal (cause : DetachedBridgeRecoveryCause) :
    isTerminal cause.terminalState := by
  cases cause <;>
    simp [terminalState, HasTerminal.isTerminal, ToolCallState.instHasTerminal]

end DetachedBridgeRecoveryCause

structure DetachedBridgeRecoveryRow where
  call : ToolCallContext
  cause : DetachedBridgeRecoveryCause
  deriving Repr

def detachedBridgeRecoveryStale (row : DetachedBridgeRecoveryRow) : Prop :=
  row.call.state = .running ∧ isDetachedBridgeCall row.call

instance (row : DetachedBridgeRecoveryRow) : Decidable (detachedBridgeRecoveryStale row) := by
  unfold detachedBridgeRecoveryStale
  infer_instance

def detachedBridgeRecover (row : DetachedBridgeRecoveryRow) : DetachedBridgeRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def detachedBridgeUninterruptedTerminalize
    (row : DetachedBridgeRecoveryRow) : DetachedBridgeRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def detachedBridgeRecoveryMeasure (row : DetachedBridgeRecoveryRow) : Nat :=
  if detachedBridgeRecoveryStale row then 1 else 0

theorem detachedBridgeRecover_matches_uninterrupted :
    ∀ row, detachedBridgeRecoveryStale row →
      detachedBridgeRecover row = detachedBridgeUninterruptedTerminalize row := by
  intro row _h_stale
  simp [detachedBridgeRecover, detachedBridgeUninterruptedTerminalize]

theorem detachedBridgeRecovery_stale_positive :
    ∀ row, detachedBridgeRecoveryStale row → detachedBridgeRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [detachedBridgeRecoveryMeasure, h_stale]

theorem detachedBridgeRecover_terminal :
    ∀ row, detachedBridgeRecoveryStale row → isTerminal (detachedBridgeRecover row).call.state := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [detachedBridgeRecover, DetachedBridgeRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]

theorem detachedBridgeRecover_zero :
    ∀ row, detachedBridgeRecoveryStale row → detachedBridgeRecoveryMeasure (detachedBridgeRecover row) = 0 := by
  intro row _h_stale
  have h_terminal_not_running : row.cause.terminalState ≠ .running := by
    cases row.cause <;> simp [DetachedBridgeRecoveryCause.terminalState]
  have h_not : ¬ detachedBridgeRecoveryStale (detachedBridgeRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_running, _h_detached⟩
    simp [detachedBridgeRecover] at h_running
    exact h_terminal_not_running h_running
  simp [detachedBridgeRecoveryMeasure, h_not]

def detachedBridgeRecoverySweep : RecoverySweep :=
  { Row := DetachedBridgeRecoveryRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_recover_detached_bridge_rows"
  , rustFunction := "ToolCallLifecycle::recover_all"
  , cadence := .startup
  , implementationStatus := .implemented
  , stale := detachedBridgeRecoveryStale
  , recover := detachedBridgeRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := detachedBridgeRecoveryMeasure
  , h_stale_positive := detachedBridgeRecovery_stale_positive
  , h_recover_terminal := detachedBridgeRecover_terminal
  , h_recover_zero := detachedBridgeRecover_zero
  }

def detachedBridgeRecoveryEquivalence :
    RecoveryEquivalence detachedBridgeRecoverySweep :=
  { uninterrupted := detachedBridgeUninterruptedTerminalize
  , h_recover_eq_uninterrupted := detachedBridgeRecover_matches_uninterrupted
  }

end Recovery
