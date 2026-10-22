import Proofs.Recovery.Sweeps.Requests
import Proofs.ToolExecution
import Proofs.ManagedExec.Ownership

namespace Recovery

open ToolExecution

def isDetachedCall (call : ToolCallContext) : Prop :=
  call.operation.isDetached = true

instance (call : ToolCallContext) : Decidable (isDetachedCall call) := by
  unfold isDetachedCall
  infer_instance

/-- The class of running rows one tool-call recovery sweep selects. A
plugin-job row is its own class: its terminal is the job driver's decision,
so neither the native nor the session-message sweep selects it. -/
inductive RowClass where
  | native
  | sessionMessage
  | pluginJob
  deriving DecidableEq, Repr

def RowClass.of (call : ToolCallContext) : RowClass :=
  match call.operation with
  | .sessionMessage => .sessionMessage
  | .pluginJob => .pluginJob
  | .mcpListTools => .native
  | .mcpCall => .native
  | .nativeCommand => .native

inductive ToolRecoveryCause where
  | deadlineExceeded
  | parentInterrupted
  | parentTerminal
  | terminalizeBackgroundedAsInterrupted
  | processLost
  | taskDeleted
  | sessionStopped
  | causedRequestUnbound
  | requestCompleted
  | requestFailed
  | requestDead
  | requestInterrupted
  | requestSuperseded
  deriving DecidableEq, Repr

namespace ToolRecoveryCause

def toContract : ToolRecoveryCause → String
  | .deadlineExceeded => "deadlineExceeded"
  | .parentInterrupted => "parentInterrupted"
  | .parentTerminal => "parentTerminal"
  | .terminalizeBackgroundedAsInterrupted => "TerminalizeBackgroundedAsInterrupted"
  | .processLost => "processLost"
  | .sessionStopped => "sessionStopped"
  | .taskDeleted => "taskDeleted"
  | .causedRequestUnbound => "causedRequestUnbound"
  | .requestCompleted => "requestCompleted"
  | .requestFailed => "requestFailed"
  | .requestDead => "requestDead"
  | .requestInterrupted => "requestInterrupted"
  | .requestSuperseded => "requestSuperseded"

def terminalState : ToolRecoveryCause → ToolCallState
  | .deadlineExceeded => .timedOut
  | .parentInterrupted => .cancelled
  | .parentTerminal => .failed
  | .terminalizeBackgroundedAsInterrupted => .cancelled
  | .processLost => .failed
  | .sessionStopped => .cancelled
  | .taskDeleted => .cancelled
  | .causedRequestUnbound => .failed
  | .requestCompleted => .completed
  | .requestFailed => .failed
  | .requestDead => .failed
  | .requestInterrupted => .cancelled
  | .requestSuperseded => .failed

/-- The `<reason>` of the completion notification the recovering owner appends. -/
def notificationReason : ToolRecoveryCause → Option String
  | .deadlineExceeded => some "deadline_exceeded"
  | .parentInterrupted => some "parent_interrupted"
  | .parentTerminal => some "parent_terminal"
  | .terminalizeBackgroundedAsInterrupted => some "interrupted_on_restart"
  | .processLost => some "process_lost"
  | .sessionStopped => some "session_stopped"
  | .taskDeleted => some "task_deleted"
  | .causedRequestUnbound => some "caused_request_unbound"
  | .requestCompleted => none
  | .requestFailed => some "request_failed"
  | .requestDead => some "request_dead"
  | .requestInterrupted => some "request_interrupted"
  | .requestSuperseded => some "request_superseded"

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

def toolCallRecoveryStale (k : RowClass) (row : ToolCallRecoveryRow) : Prop :=
  row.call.state = .running ∧ RowClass.of row.call = k

instance (k : RowClass) (row : ToolCallRecoveryRow) :
    Decidable (toolCallRecoveryStale k row) := by
  unfold toolCallRecoveryStale
  infer_instance

def toolCallRecover (row : ToolCallRecoveryRow) : ToolCallRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def toolCallRecoveryMeasure (k : RowClass) (row : ToolCallRecoveryRow) : Nat :=
  if toolCallRecoveryStale k row then 1 else 0

theorem toolCallRecovery_stale_positive (k : RowClass) :
    ∀ row, toolCallRecoveryStale k row →
      toolCallRecoveryMeasure k row > 0 := by
  intro row h_stale
  simp [toolCallRecoveryMeasure, h_stale]

theorem toolCallRecover_terminal (k : RowClass) :
    ∀ row, toolCallRecoveryStale k row →
      isTerminal (toolCallRecover row).call.state := by
  intro row _h_stale
  exact row.cause.terminalState_terminal

theorem toolCallRecover_zero (k : RowClass) :
    ∀ row, toolCallRecoveryStale k row →
      toolCallRecoveryMeasure k (toolCallRecover row) = 0 := by
  intro row _h_stale
  have h_terminal_not_running : row.cause.terminalState ≠ .running := by
    cases row.cause <;> simp [ToolRecoveryCause.terminalState]
  have h_not : ¬ toolCallRecoveryStale k (toolCallRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_running, _h_session⟩
    simp [toolCallRecover] at h_running
    exact h_terminal_not_running h_running
  simp [toolCallRecoveryMeasure, h_not]

def toolCallRecoverySweepFor (k : RowClass)
    (sweepId rustFunction : String) (cadence : RecoveryCadence) : RecoverySweep :=
  { Row := ToolCallRecoveryRow
  , collection := .agentToolCall
  , sweepId := sweepId
  , rustFunction := rustFunction
  , cadence := cadence
  , implementationStatus := .implemented
  , stale := toolCallRecoveryStale k
  , recover := toolCallRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := toolCallRecoveryMeasure k
  , h_stale_positive := toolCallRecovery_stale_positive k
  , h_recover_terminal := toolCallRecover_terminal k
  , h_recover_zero := toolCallRecover_zero k
  }

def toolCallRecoverySweep : RecoverySweep :=
  toolCallRecoverySweepFor .native
    "tool_call_lifecycle_recover_all_running_calls" "ToolCallLifecycle::recover_all" .startup

/-- A plugin-job row's terminal is the job driver's decision: the startup
    native sweep never selects it, so a restart leaves it running. -/
theorem native_sweep_never_selects_plugin_job
    (row : ToolCallRecoveryRow) (h_job : row.call.operation = .pluginJob) :
    ¬ toolCallRecoveryStale .native row := by
  simp [toolCallRecoveryStale, RowClass.of, h_job]

structure OrphanedBackgroundToolRow where
  call : ToolCallContext
  deadlineExpired : Bool
  parentLive : Bool
  parentInterrupted : Bool
  parentTerminal : Bool
  executionRegistered : Bool
  process : ManagedExec.StopOutcome
  ownerTaskDeleted : Bool
  ownerSessionStopped : Bool := false
  deriving Repr

def OrphanedBackgroundToolRow.parentResolvable
    (row : OrphanedBackgroundToolRow) : Bool :=
  row.parentLive || row.parentInterrupted || row.parentTerminal

/-! The periodic orphan sweep uses the same precedence as startup recovery.
Session-stop recovery signals a registered process first and only records a
terminal cause after observing it stopped or no longer owned. Task deletion
retains its existing registered-worker ordering. Parent interruption and
terminal state do not stop background work. -/

def orphanedBackgroundToolCause
    (row : OrphanedBackgroundToolRow) : Option ToolRecoveryCause :=
  if !row.parentResolvable then
    none
  else if row.executionRegistered then
    if row.ownerSessionStopped then
      match row.process with
      | .stillRunning => none
      | .notOwned => some .processLost
      | .alreadyExited => some .processLost
      | .stopped => some .sessionStopped
    else if row.ownerTaskDeleted then some .taskDeleted else none
  else
    match row.process with
    | .stillRunning => none
    | .notOwned => some .processLost
    | .alreadyExited => some .processLost
    | .stopped =>
      if row.deadlineExpired then
        some .deadlineExceeded
      else if row.ownerSessionStopped then
        some .sessionStopped
      else if row.ownerTaskDeleted then
        some .taskDeleted
      else
        some .terminalizeBackgroundedAsInterrupted

theorem orphanedBackgroundTool_no_parent_no_cause
    (row : OrphanedBackgroundToolRow)
    (h : row.parentResolvable = false) :
    orphanedBackgroundToolCause row = none := by
  simp [orphanedBackgroundToolCause, h]

theorem orphanedBackgroundTool_unstopped_settles_lost
    (row : OrphanedBackgroundToolRow) (cause : ToolRecoveryCause)
    (h_cause : orphanedBackgroundToolCause row = some cause)
    (h_unregistered : row.executionRegistered = false)
    (h_process : row.process ≠ .stopped) :
    cause = .processLost := by
  unfold orphanedBackgroundToolCause at h_cause
  cases h_resolvable : row.parentResolvable <;>
    cases h_registered : row.executionRegistered <;>
    cases h_stopped : row.ownerSessionStopped <;>
    cases h_deleted : row.ownerTaskDeleted <;>
    cases h_p : row.process <;>
    simp_all

theorem orphanedBackgroundTool_registered_live_worker_untouched
    (row : OrphanedBackgroundToolRow)
    (h_registered : row.executionRegistered = true)
    (h_deleted : row.ownerTaskDeleted = false)
    (h_stopped : row.ownerSessionStopped = false) :
    orphanedBackgroundToolCause row = none := by
  simp [orphanedBackgroundToolCause, h_registered, h_deleted, h_stopped]

def orphanedBackgroundToolStale (row : OrphanedBackgroundToolRow) : Prop :=
  row.call.state = .running ∧
  row.call.awaitMode = .background ∧
  ¬ isDetachedCall row.call ∧
  (row.executionRegistered = false ∨ row.ownerTaskDeleted = true ∨
    row.ownerSessionStopped = true) ∧
  (orphanedBackgroundToolCause row).isSome = true

instance (row : OrphanedBackgroundToolRow) :
    Decidable (orphanedBackgroundToolStale row) := by
  unfold orphanedBackgroundToolStale
  infer_instance

/-- A plugin-job row has no host process to orphan; its terminal is the job
    driver's decision, so the orphan sweep never selects it. -/
theorem orphan_sweep_never_selects_plugin_job
    (row : OrphanedBackgroundToolRow)
    (h_job : row.call.operation = .pluginJob) :
    ¬ orphanedBackgroundToolStale row := by
  intro h_stale
  exact h_stale.2.2.1 (by simp [isDetachedCall, h_job])

def orphanedBackgroundToolRecover
    (row : OrphanedBackgroundToolRow) : OrphanedBackgroundToolRow :=
  match orphanedBackgroundToolCause row with
  | some cause => { row with call := { row.call with state := cause.terminalState } }
  | none => row

theorem orphanedBackgroundTool_no_parent_no_terminal
    (row : OrphanedBackgroundToolRow)
    (h : row.parentResolvable = false) :
    orphanedBackgroundToolRecover row = row ∧
      ¬ orphanedBackgroundToolStale row := by
  have hc := orphanedBackgroundTool_no_parent_no_cause row h
  constructor
  · simp [orphanedBackgroundToolRecover, hc]
  · simp [orphanedBackgroundToolStale, hc]

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

/-! The persisted `completionPending` cursor stays outstanding until both
idempotent side effects converge, keeping transient write failures discoverable
after the lifecycle row is terminal. -/

structure BackgroundCompletionSideEffectRow where
  call : ToolCallContext
  parentResolvable : Bool
  sideEffectsDone : Bool
  deriving Repr

def isNativeBackgroundCall (call : ToolCallContext) : Prop :=
  call.awaitMode = .background ∧ ¬ isDetachedCall call

def isBackgroundCall (call : ToolCallContext) : Prop :=
  call.awaitMode = .background

instance (call : ToolCallContext) : Decidable (isBackgroundCall call) := by
  unfold isBackgroundCall
  infer_instance

instance (call : ToolCallContext) : Decidable (isNativeBackgroundCall call) := by
  unfold isNativeBackgroundCall
  infer_instance

def backgroundCompletionSideEffectStale
    (row : BackgroundCompletionSideEffectRow) : Prop :=
  isTerminal row.call.state ∧
  isBackgroundCall row.call ∧
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

structure TerminalParentToolRow where
  call : ToolCallContext
  parentTerminal : Bool
  parentInterrupted : Bool
  deriving Repr

/-- No terminal parent cancels background work; only foreground rows are
terminalized by this sweep. -/
def terminalParentToolStale (row : TerminalParentToolRow) : Prop :=
  row.call.state = .running ∧
  (row.parentInterrupted = true ∨ row.parentTerminal = true) ∧
  ¬ isBackgroundCall row.call

theorem terminalParent_background_not_stale
    (row : TerminalParentToolRow)
    (h_background : isBackgroundCall row.call) :
    ¬ terminalParentToolStale row := by
  intro h_stale
  exact h_stale.2.2 h_background

instance (row : TerminalParentToolRow) : Decidable (terminalParentToolStale row) := by
  unfold terminalParentToolStale
  infer_instance

def terminalParentToolRecover (row : TerminalParentToolRow) : TerminalParentToolRow :=
  let cause : ToolRecoveryCause :=
    if row.parentInterrupted then .parentInterrupted else .parentTerminal
  { row with call := { row.call with state := cause.terminalState } }

def terminalParentToolMeasure (row : TerminalParentToolRow) : Nat :=
  if terminalParentToolStale row then 1 else 0

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
      rcases h_stale with ⟨_, h_parent, _⟩
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
    rcases h with ⟨h_running, _, _⟩
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

end Recovery
