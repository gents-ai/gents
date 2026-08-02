import Proofs.Recovery.Sweeps.RequestResponse
import Proofs.ToolExecution

namespace Recovery

open ToolExecution

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

/-! ## Periodic native-background ownership repair

A native background row is backed by volatile process state while its worker
is alive. If the durable row is still running but the process registry no
longer owns its id, the row is orphaned and the startup classifier must be
re-applied on the periodic recovery tick. This makes startup recovery retryable
and closes the panic path without touching live registered workers. -/

structure OrphanedBackgroundToolRow where
  call : ToolCallContext
  cause : ToolRecoveryCause
  executionRegistered : Bool
  parentResolvable : Bool
  deriving Repr

def orphanedBackgroundToolStale (row : OrphanedBackgroundToolRow) : Prop :=
  row.call.state = .running ∧
  row.call.awaitMode = .background ∧
  row.call.childRequestId = none ∧
  row.executionRegistered = false ∧
  row.parentResolvable = true

instance (row : OrphanedBackgroundToolRow) :
    Decidable (orphanedBackgroundToolStale row) := by
  unfold orphanedBackgroundToolStale
  infer_instance

def orphanedBackgroundToolRecover
    (row : OrphanedBackgroundToolRow) : OrphanedBackgroundToolRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def orphanedBackgroundToolMeasure (row : OrphanedBackgroundToolRow) : Nat :=
  if orphanedBackgroundToolStale row then 1 else 0

theorem orphanedBackgroundTool_stale_positive :
    ∀ row, orphanedBackgroundToolStale row →
      orphanedBackgroundToolMeasure row > 0 := by
  intro row h_stale
  simp [orphanedBackgroundToolMeasure, h_stale]

theorem orphanedBackgroundToolRecover_terminal :
    ∀ row, orphanedBackgroundToolStale row →
      isTerminal (orphanedBackgroundToolRecover row).call.state := by
  intro row _h_stale
  simpa [orphanedBackgroundToolRecover] using row.cause.terminalState_terminal

theorem orphanedBackgroundToolRecover_zero :
    ∀ row, orphanedBackgroundToolStale row →
      orphanedBackgroundToolMeasure (orphanedBackgroundToolRecover row) = 0 := by
  intro row _h_stale
  have h_terminal_not_running : row.cause.terminalState ≠ .running := by
    cases row.cause <;> simp [ToolRecoveryCause.terminalState]
  have h_not :
      ¬ orphanedBackgroundToolStale (orphanedBackgroundToolRecover row) := by
    intro h_stale
    exact h_terminal_not_running h_stale.1
  simp [orphanedBackgroundToolMeasure, h_not]

def orphanedBackgroundToolSweep : RecoverySweep :=
  { Row := OrphanedBackgroundToolRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_reconcile_orphaned_background_tools"
  , rustFunction := "ToolCallLifecycle::reconcile_orphaned_background_tools"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := orphanedBackgroundToolStale
  , recover := orphanedBackgroundToolRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := orphanedBackgroundToolMeasure
  , h_stale_positive := orphanedBackgroundTool_stale_positive
  , h_recover_terminal := orphanedBackgroundToolRecover_terminal
  , h_recover_zero := orphanedBackgroundToolRecover_zero
  }

def orphanedBackgroundToolUninterruptedTerminalize
    (row : OrphanedBackgroundToolRow) : OrphanedBackgroundToolRow :=
  orphanedBackgroundToolRecover row

theorem orphanedBackgroundToolRecover_matches_uninterrupted :
    ∀ row, orphanedBackgroundToolStale row →
      orphanedBackgroundToolRecover row =
        orphanedBackgroundToolUninterruptedTerminalize row := by
  intro _row _h_stale
  rfl

def orphanedBackgroundToolEquivalence :
    RecoveryEquivalence orphanedBackgroundToolSweep :=
  { uninterrupted := orphanedBackgroundToolUninterruptedTerminalize
  , h_recover_eq_uninterrupted :=
      orphanedBackgroundToolRecover_matches_uninterrupted
  }

structure TerminalParentToolRow where
  call : ToolCallContext
  parentTerminal : Bool
  parentInterrupted : Bool
  parentCleanCompleted : Bool
  deriving Repr

def isChildLinkedBridge (call : ToolCallContext) : Prop :=
  call.childRequestId.isSome

instance (call : ToolCallContext) : Decidable (isChildLinkedBridge call) := by
  unfold isChildLinkedBridge
  infer_instance

def exclusiveCleanCompleted (row : TerminalParentToolRow) : Prop :=
  row.parentCleanCompleted = true ∧ row.parentInterrupted = false

instance (row : TerminalParentToolRow) : Decidable (exclusiveCleanCompleted row) := by
  unfold exclusiveCleanCompleted
  infer_instance

def terminalParentToolStale (row : TerminalParentToolRow) : Prop :=
  row.call.state = .running ∧
  (row.parentInterrupted = true ∨ row.parentTerminal = true) ∧
  ¬ (isDetachedBridgeCall row.call ∧ row.parentInterrupted = true) ∧
  ¬ (isChildLinkedBridge row.call ∧ exclusiveCleanCompleted row)

instance (row : TerminalParentToolRow) : Decidable (terminalParentToolStale row) := by
  unfold terminalParentToolStale
  infer_instance

def terminalParentToolRecover (row : TerminalParentToolRow) : TerminalParentToolRow :=
  let cause : ToolRecoveryCause :=
    if row.parentInterrupted then .parentInterrupted else .parentTerminal
  { row with call := { row.call with state := cause.terminalState } }

def terminalParentToolUninterruptedTerminalize
    (row : TerminalParentToolRow) : TerminalParentToolRow :=
  terminalParentToolRecover row

def terminalParentToolMeasure (row : TerminalParentToolRow) : Nat :=
  if terminalParentToolStale row then 1 else 0

theorem terminalParentToolRecover_matches_uninterrupted :
    ∀ row, terminalParentToolStale row →
      terminalParentToolRecover row = terminalParentToolUninterruptedTerminalize row := by
  intro row _h_stale
  rfl

theorem terminalParentTool_stale_positive :
    ∀ row, terminalParentToolStale row → terminalParentToolMeasure row > 0 := by
  intro row h_stale
  simp [terminalParentToolMeasure, h_stale]

theorem terminalParentToolRecover_terminal :
    ∀ row, terminalParentToolStale row →
      isTerminal (terminalParentToolRecover row).call.state := by
  intro row h_stale
  unfold terminalParentToolRecover
  by_cases h_int : row.parentInterrupted
  · simp [h_int, ToolRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]
  ·
    have h_term : row.parentTerminal = true := by
      rcases h_stale with ⟨_, h_parent, _, _⟩
      cases h_parent with
      | inl h => exact absurd h (by simpa using h_int)
      | inr h => exact h
    simp [h_int, h_term, ToolRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]

theorem terminalParentToolRecover_zero :
    ∀ row, terminalParentToolStale row →
      terminalParentToolMeasure (terminalParentToolRecover row) = 0 := by
  intro row h_stale
  have h_not : ¬ terminalParentToolStale (terminalParentToolRecover row) := by
    intro h
    rcases h with ⟨h_running, _, _, _⟩
    unfold terminalParentToolRecover at h_running
    by_cases h_int : row.parentInterrupted
    · simp [h_int, ToolRecoveryCause.terminalState] at h_running
    · simp [h_int, ToolRecoveryCause.terminalState] at h_running
  simp [terminalParentToolMeasure, h_not]

def terminalParentOwnedToolSweep : RecoverySweep :=
  { Row := TerminalParentToolRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_reconcile_terminal_parent_owned_tools"
  , rustFunction := "ToolCallLifecycle::reconcile_terminal_parent_owned_tools"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := terminalParentToolStale
  , recover := terminalParentToolRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := terminalParentToolMeasure
  , h_stale_positive := terminalParentTool_stale_positive
  , h_recover_terminal := terminalParentToolRecover_terminal
  , h_recover_zero := terminalParentToolRecover_zero
  }

def terminalParentOwnedToolEquivalence :
    RecoveryEquivalence terminalParentOwnedToolSweep :=
  { uninterrupted := terminalParentToolUninterruptedTerminalize
  , h_recover_eq_uninterrupted := terminalParentToolRecover_matches_uninterrupted
  }

end Recovery
