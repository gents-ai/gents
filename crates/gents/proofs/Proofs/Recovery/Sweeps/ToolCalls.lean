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
  | preDispatchFailure
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
  | .preDispatchFailure => "preDispatchFailure"
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
  | .preDispatchFailure => .failed
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

/-- The parent observation used only while deciding whether a durable pending
    tool execution is abandoned. `missingOrForeign` deliberately combines a
    missing row with a row outside the tool's immutable owner DID: neither is
    authority to terminate before the tool's explicit deadline. -/
inductive PendingParentObservation where
  | missingOrForeign
  | live
  | interrupted
  | otherTerminal
  deriving DecidableEq, Repr

structure ToolCallRecoveryRow where
  call : ToolCallContext
  cause : ToolRecoveryCause
  pendingParent : PendingParentObservation
  deriving Repr

/-- Pending recovery is selected from durable facts, never merely from seeing
    a pending row at startup. Deadline expiry has precedence. Before expiry,
    only a same-owner terminal parent proves that dispatch can no longer be
    live on another deployment. -/
def pendingToolRecoveryCause
    (row : ToolCallRecoveryRow) : Option ToolRecoveryCause :=
  if row.call.deadlineExceeded then
    some .preDispatchFailure
  else
    match row.pendingParent with
    | .interrupted => some .parentInterrupted
    | .otherTerminal => some .preDispatchFailure
    | .missingOrForeign | .live => none

def toolCallRecoveryStale (row : ToolCallRecoveryRow) : Prop :=
  (row.call.state = .pending ∧ pendingToolRecoveryCause row = some row.cause) ∨
  (row.call.state = .running ∧ ¬ isDetachedBridgeCall row.call)

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
  rcases row with ⟨call, cause, pendingParent⟩
  cases cause <;>
    simp [toolCallRecover, ToolRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]

theorem toolCallRecover_zero :
    ∀ row, toolCallRecoveryStale row → toolCallRecoveryMeasure (toolCallRecover row) = 0 := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [toolCallRecoveryMeasure, toolCallRecoveryStale, toolCallRecover,
      ToolRecoveryCause.terminalState]

private def pendingRecoveryFixture
    (currentTime : Time)
    (parent : PendingParentObservation)
    (cause : ToolRecoveryCause) : ToolCallRecoveryRow :=
  { call :=
      { callId := 501
      , requestId := 601
      , state := .pending
      , operation := .nativeCommand
      , deadline := 100
      , currentTime := currentTime
      , persistence := .committed
      }
  , cause := cause
  , pendingParent := parent
  }

/-- A live same-owner parent and an active deadline preserve pending dispatch:
    another deployment may still legally advance it. -/
theorem pending_live_parent_active_lease_not_stale :
    ¬ toolCallRecoveryStale
      (pendingRecoveryFixture 10 .live .preDispatchFailure) := by
  native_decide

/-- Missing/foreign parent observation is not proof of abandonment while the
    explicit dispatch lease remains active. -/
theorem pending_missing_parent_active_lease_not_stale :
    ¬ toolCallRecoveryStale
      (pendingRecoveryFixture 10 .missingOrForeign .preDispatchFailure) := by
  native_decide

/-- Once the explicit deadline expires, even a missing/foreign-parent row is
    safely recoverable and deadline classification has precedence. -/
theorem pending_orphan_expired_lease_stale :
    toolCallRecoveryStale
      (pendingRecoveryFixture 101 .missingOrForeign .preDispatchFailure) := by
  native_decide

/-- A same-owner interrupted parent proves that pending dispatch is abandoned
    without waiting for the deadline. -/
theorem pending_interrupted_parent_stale :
    toolCallRecoveryStale
      (pendingRecoveryFixture 10 .interrupted .parentInterrupted) := by
  native_decide

/-! ## Fork-copy staging lease

Terminal fork copies are first created in a non-terminal `forkStaging` phase
and then atomically bound to copied exact evidence. Recovery may cancel that
staging row only from an explicit expired lease; missing or unreadable lease
data is not evidence that a concurrent fork writer has stopped. -/

structure ForkStagingRecoveryRow where
  call : ToolCallContext
  forkStaging : Bool
  sourceBound : Bool
  leaseExpired : Option Bool
  deriving Repr

def forkStagingRecoveryEligible (row : ForkStagingRecoveryRow) : Prop :=
  ¬ isTerminal row.call.state ∧ row.forkStaging = true ∧
    row.sourceBound = true ∧ row.leaseExpired = some true

instance (row : ForkStagingRecoveryRow) :
    Decidable (forkStagingRecoveryEligible row) := by
  unfold forkStagingRecoveryEligible
  infer_instance

private def forkStagingFixture (leaseExpired : Option Bool) :
    ForkStagingRecoveryRow :=
  { call := (pendingRecoveryFixture 10 .missingOrForeign .preDispatchFailure).call
  , forkStaging := true
  , sourceBound := true
  , leaseExpired := leaseExpired
  }

theorem fork_staging_expired_lease_recoverable :
    forkStagingRecoveryEligible (forkStagingFixture (some true)) := by
  native_decide

theorem fork_staging_active_lease_not_recoverable :
    ¬ forkStagingRecoveryEligible (forkStagingFixture (some false)) := by
  native_decide

theorem fork_staging_missing_lease_not_recoverable :
    ¬ forkStagingRecoveryEligible (forkStagingFixture none) := by
  native_decide

def toolCallRecoverySweep : RecoverySweep :=
  { Row := ToolCallRecoveryRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_recover_all_incomplete_calls"
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
  deadlineExpired : Bool
  unclaimedExpired : Bool
  parentLive : Bool
  parentInterrupted : Bool
  parentTerminal : Bool
  executionRegistered : Bool
  deriving Repr

/-- The periodic orphan sweep uses the same precedence as startup recovery.
    Parent flags are observations, not a caller-selected recovery cause. -/
def orphanedBackgroundToolCause
    (row : OrphanedBackgroundToolRow) : Option ToolRecoveryCause :=
  if row.deadlineExpired then
    some .deadlineExceeded
  else if row.unclaimedExpired then
    some .unclaimedCrossDeploymentSpawn
  else if row.parentLive then
    some .terminalizeBackgroundedAsInterrupted
  else if row.parentInterrupted then
    some .parentInterrupted
  else if row.parentTerminal then
    some .parentTerminal
  else
    none

def orphanedBackgroundToolStale (row : OrphanedBackgroundToolRow) : Prop :=
  row.call.state = .running ∧
  row.call.awaitMode = .background ∧
  row.call.childRequestId = none ∧
  row.executionRegistered = false ∧
  (orphanedBackgroundToolCause row).isSome = true

instance (row : OrphanedBackgroundToolRow) :
    Decidable (orphanedBackgroundToolStale row) := by
  unfold orphanedBackgroundToolStale
  infer_instance

def orphanedBackgroundToolRecover
    (row : OrphanedBackgroundToolRow) : OrphanedBackgroundToolRow :=
  match orphanedBackgroundToolCause row with
  | some cause => { row with call := { row.call with state := cause.terminalState } }
  | none => row

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
  intro row h_stale
  rcases h_stale with ⟨_, _, _, _, h_cause⟩
  cases h : orphanedBackgroundToolCause row with
  | none => simp [h] at h_cause
  | some cause =>
      simpa [orphanedBackgroundToolRecover, h] using cause.terminalState_terminal

theorem orphanedBackgroundToolRecover_zero :
    ∀ row, orphanedBackgroundToolStale row →
      orphanedBackgroundToolMeasure (orphanedBackgroundToolRecover row) = 0 := by
  intro row h_stale
  rcases h_stale with ⟨_, _, _, _, h_cause⟩
  cases h : orphanedBackgroundToolCause row with
  | none => simp [h] at h_cause
  | some cause =>
    have h_terminal_not_running : cause.terminalState ≠ .running := by
      cases cause <;> simp [ToolRecoveryCause.terminalState]
    have h_not :
        ¬ orphanedBackgroundToolStale (orphanedBackgroundToolRecover row) := by
      intro recovered_stale
      exact h_terminal_not_running (by
        simpa [orphanedBackgroundToolRecover, h] using recovered_stale.1)
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

/-! ## Retryable native-background completion side effects

The terminal tool row is durable before its completion notification and wake.
The persisted `status = completionPending` cursor therefore remains outstanding
until both idempotent side effects converge, making a transient write failure
periodically discoverable after the lifecycle row is already terminal. -/

structure BackgroundCompletionSideEffectRow where
  call : ToolCallContext
  parentResolvable : Bool
  sideEffectsDone : Bool
  deriving Repr

def isNativeBackgroundCall (call : ToolCallContext) : Prop :=
  call.awaitMode = .background ∧ call.childRequestId = none

instance (call : ToolCallContext) : Decidable (isNativeBackgroundCall call) := by
  unfold isNativeBackgroundCall
  infer_instance

def backgroundCompletionSideEffectStale
    (row : BackgroundCompletionSideEffectRow) : Prop :=
  isTerminal row.call.state ∧
  isNativeBackgroundCall row.call ∧
  row.parentResolvable = true ∧
  row.sideEffectsDone = false

instance (row : BackgroundCompletionSideEffectRow) :
    Decidable (backgroundCompletionSideEffectStale row) := by
  unfold backgroundCompletionSideEffectStale
  infer_instance

def backgroundCompletionSideEffectRecover
    (row : BackgroundCompletionSideEffectRow) : BackgroundCompletionSideEffectRow :=
  { row with sideEffectsDone := true }

def backgroundCompletionSideEffectMeasure
    (row : BackgroundCompletionSideEffectRow) : Nat :=
  if backgroundCompletionSideEffectStale row then 1 else 0

theorem backgroundCompletionSideEffect_stale_positive :
    ∀ row, backgroundCompletionSideEffectStale row →
      backgroundCompletionSideEffectMeasure row > 0 := by
  intro row h_stale
  simp [backgroundCompletionSideEffectMeasure, h_stale]

theorem backgroundCompletionSideEffectRecover_terminal :
    ∀ row, backgroundCompletionSideEffectStale row →
      isTerminal (backgroundCompletionSideEffectRecover row).call.state := by
  intro _row h_stale
  exact h_stale.1

theorem backgroundCompletionSideEffectRecover_zero :
    ∀ row, backgroundCompletionSideEffectStale row →
      backgroundCompletionSideEffectMeasure
        (backgroundCompletionSideEffectRecover row) = 0 := by
  intro row _h_stale
  have h_not : ¬ backgroundCompletionSideEffectStale
      (backgroundCompletionSideEffectRecover row) := by
    intro h
    rcases h with ⟨_, _, _, h_done⟩
    simp [backgroundCompletionSideEffectRecover] at h_done
  simp [backgroundCompletionSideEffectMeasure, h_not]

def backgroundCompletionSideEffectSweep : RecoverySweep :=
  { Row := BackgroundCompletionSideEffectRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_reconcile_background_completion_side_effects"
  , rustFunction := "ToolCallLifecycle::reconcile_background_completion_side_effects"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := backgroundCompletionSideEffectStale
  , recover := backgroundCompletionSideEffectRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := backgroundCompletionSideEffectMeasure
  , h_stale_positive := backgroundCompletionSideEffect_stale_positive
  , h_recover_terminal := backgroundCompletionSideEffectRecover_terminal
  , h_recover_zero := backgroundCompletionSideEffectRecover_zero
  }

def backgroundCompletionSideEffectUninterrupted
    (row : BackgroundCompletionSideEffectRow) : BackgroundCompletionSideEffectRow :=
  backgroundCompletionSideEffectRecover row

theorem backgroundCompletionSideEffectRecover_matches_uninterrupted :
    ∀ row, backgroundCompletionSideEffectStale row →
      backgroundCompletionSideEffectRecover row =
        backgroundCompletionSideEffectUninterrupted row := by
  intro _row _h_stale
  rfl

def backgroundCompletionSideEffectEquivalence :
    RecoveryEquivalence backgroundCompletionSideEffectSweep :=
  { uninterrupted := backgroundCompletionSideEffectUninterrupted
  , h_recover_eq_uninterrupted :=
      backgroundCompletionSideEffectRecover_matches_uninterrupted
  }

/- Native background calls are disjoint from this sweep: the orphan sweep owns
   their volatile-registration gate and deadline/unclaimed precedence. -/
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
  ¬ (isChildLinkedBridge row.call ∧ exclusiveCleanCompleted row) ∧
  ¬ isNativeBackgroundCall row.call

theorem terminalParent_native_background_not_stale
    (row : TerminalParentToolRow)
    (h_native : isNativeBackgroundCall row.call) :
    ¬ terminalParentToolStale row := by
  intro h_stale
  exact h_stale.2.2.2.2 h_native

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
      rcases h_stale with ⟨_, h_parent, _, _, _⟩
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
    rcases h with ⟨h_running, _, _, _, _⟩
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
