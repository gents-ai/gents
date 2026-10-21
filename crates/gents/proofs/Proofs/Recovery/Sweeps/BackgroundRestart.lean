import Proofs.Recovery.Sweeps.ToolCalls

/-! Models `ToolCallLifecycle::recover_all` startup classification. Native
background rows use the host stop verdict and require durable completion
notification after terminalization. Missing parents defer writes; session
message rows remain running for their caused-request completion observer, and
plugin-job rows remain running for their job driver. -/

namespace Recovery

open ToolExecution

/-- `missing`: the exact physical request document cannot be resolved in the
recovering node's scope. Replication may supply that owner later; absent owner
facts are never grounds for a local write. -/
inductive ParentObservation where
  | live
  | interrupted
  | cleanlyCompleted
  | otherTerminal
  | missing
  deriving DecidableEq, Repr

namespace ParentObservation

def toContract : ParentObservation → String
  | .live => "live"
  | .interrupted => "interrupted"
  | .cleanlyCompleted => "cleanlyCompleted"
  | .otherTerminal => "otherTerminal"
  | .missing => "missing"

/-- `missing` is not terminal: an unresolvable parent never grounds a local
terminalization. -/
def observedTerminal : ParentObservation → Prop
  | .live => False
  | .missing => False
  | .interrupted => True
  | .cleanlyCompleted => True
  | .otherTerminal => True

instance (p : ParentObservation) : Decidable p.observedTerminal := by
  cases p <;> simp [observedTerminal] <;> infer_instance

def all : List ParentObservation :=
  [ .live, .interrupted, .cleanlyCompleted, .otherTerminal, .missing ]

theorem all_complete (p : ParentObservation) : p ∈ all := by
  cases p <;> simp [all]

end ParentObservation

/-- The tool kind of a running row: `sessionMessage` is the
    `agent_new`/`agent_message` kind, `pluginJob` a plugin job; neither is
    backed by a host process. -/
inductive RowKind where
  | native
  | sessionMessage
  | pluginJob
  deriving DecidableEq, Repr

/-- One running `AgentToolCall` row as the startup classifier sees it
    (`lifecycle_state = "running"`, so the state itself is not a field). -/
structure RestartRow where
  awaitMode : AwaitMode
  kind : RowKind
  parent : ParentObservation
  deadlineExpired : Bool
  /-- Host stop verdict for a native background process; other rows carry
      `stopped` and ignore it. -/
  process : ManagedExec.StopOutcome
  deriving DecidableEq, Repr

inductive RestartDisposition where
  | terminalize (cause : ToolRecoveryCause)
  | leaveRunning
  deriving DecidableEq, Repr

namespace RestartDisposition

def toContract : RestartDisposition → String
  | .terminalize _ => "terminalize"
  | .leaveRunning => "leave_running"

def causeContract : RestartDisposition → Option String
  | .terminalize cause => some cause.toContract
  | _ => none

def terminalStateContract : RestartDisposition → Option String
  | .terminalize cause => some cause.terminalState.toDefraDB
  | _ => none

end RestartDisposition

def RestartRow.isNativeBackgroundTool (row : RestartRow) : Prop :=
  row.awaitMode = .background ∧ row.kind = .native

instance (row : RestartRow) : Decidable row.isNativeBackgroundTool := by
  unfold RestartRow.isNativeBackgroundTool
  infer_instance

/-- Branch order is the production order in `recover_stuck_running_tool_calls`;
reordering any two branches changes the value on some row and fails the
exhaustive theorems below. -/
def restartDisposition (row : RestartRow) : RestartDisposition :=
  if row.parent = .missing then
    .leaveRunning
  else if row.kind ≠ .native then
    .leaveRunning
  else if row.isNativeBackgroundTool ∧ row.process = .stillRunning then
    .leaveRunning
  else if row.isNativeBackgroundTool ∧ row.process ≠ .stopped then
    .terminalize .processLost
  else if row.deadlineExpired then
    .terminalize .deadlineExceeded
  else if row.isNativeBackgroundTool then
    .terminalize .terminalizeBackgroundedAsInterrupted
  else if row.parent = .interrupted then
    .terminalize .parentInterrupted
  else if row.parent.observedTerminal then
    .terminalize .parentTerminal
  else
    .leaveRunning

/-- Boolean priority matches `orphanedBackgroundToolCause`. -/
def OrphanedBackgroundToolRow.parentObservation
    (row : OrphanedBackgroundToolRow) : ParentObservation :=
  if row.parentLive then .live
  else if row.parentInterrupted then .interrupted
  else if row.parentTerminal then .otherTerminal
  else .missing

def OrphanedBackgroundToolRow.toRestartRow
    (row : OrphanedBackgroundToolRow) : RestartRow :=
  { awaitMode := row.call.awaitMode
  , kind :=
      if row.call.operation = .sessionMessage then .sessionMessage
      else if row.call.operation = .pluginJob then .pluginJob
      else .native
  , parent := row.parentObservation
  , deadlineExpired := row.deadlineExpired
  , process := row.process
  }

theorem orphanedBackgroundToolCause_matches_restartDisposition
    (row : OrphanedBackgroundToolRow)
    (h_background : row.call.awaitMode = .background)
    (h_native : row.call.operation.isDetached = false)
    (h_unregistered : row.executionRegistered = false)
    (h_task : row.ownerTaskDeleted = false)
    (h_stop : row.ownerSessionStopped = false) :
    (orphanedBackgroundToolCause row).map ToolRecoveryCause.toContract =
      (restartDisposition row.toRestartRow).causeContract := by
  have h_session : row.call.operation ≠ .sessionMessage := fun h => by
    simp [h] at h_native
  have h_job : row.call.operation ≠ .pluginJob := fun h => by
    simp [h] at h_native
  cases h_deadline : row.deadlineExpired <;>
    cases h_live : row.parentLive <;>
    cases h_interrupted : row.parentInterrupted <;>
    cases h_terminal : row.parentTerminal <;>
    cases h_process : row.process <;>
    simp [orphanedBackgroundToolCause, OrphanedBackgroundToolRow.toRestartRow,
      OrphanedBackgroundToolRow.parentObservation,
      OrphanedBackgroundToolRow.parentResolvable, restartDisposition,
      RestartRow.isNativeBackgroundTool,
      RestartDisposition.causeContract, h_background, h_session, h_job, h_deadline,
      h_live, h_interrupted, h_terminal, h_process,
      h_unregistered, h_task, h_stop, ParentObservation.observedTerminal]

/-- Durable side effects owed after terminalizing a native background tool on
restart: cause-specific `<tool-completion>` notification reason and the
coalesced wake queue vocabulary (`background_completion:<parent session>`). -/
structure RestartNotificationObligation where
  notificationReason : String
  queueSource : String
  queueKeyPrefix : String
  deriving DecidableEq, Repr

def restartNotificationObligation
    (cause : ToolRecoveryCause) : RestartNotificationObligation :=
  { notificationReason := cause.notificationReason.getD ""
  , queueSource := "background_completion"
  , queueKeyPrefix := "background_completion:"
  }

def RestartRow.notification (row : RestartRow) :
    Option RestartNotificationObligation :=
  if row.isNativeBackgroundTool ∧ row.parent ≠ .missing then
    match restartDisposition row with
    | .terminalize cause => some (restartNotificationObligation cause)
    | _ => none
  else
    none

theorem missing_parent_never_terminalizes (row : RestartRow)
    (h : row.parent = .missing) :
    restartDisposition row = .leaveRunning ∧ row.notification = none := by
  constructor
  · simp [restartDisposition, h]
  · simp [RestartRow.notification, h]

theorem native_background_tool_interrupted_on_restart
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_owner : row.parent ≠ .missing)
    (h_deadline : row.deadlineExpired = false)
    (h_process : row.process = .stopped) :
    restartDisposition row =
      .terminalize .terminalizeBackgroundedAsInterrupted := by
  have h_session : row.kind = .native := h_native.2
  simp [restartDisposition, h_native, h_session, h_owner, h_deadline, h_process]

theorem native_background_tool_live_parent_interrupted_on_restart
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_live : row.parent = .live)
    (h_deadline : row.deadlineExpired = false)
    (h_process : row.process = .stopped) :
    restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted ∧
      (restartDisposition row).terminalStateContract = some "cancelled" ∧
      row.notification =
        some (restartNotificationObligation
          .terminalizeBackgroundedAsInterrupted) := by
  have h_session : row.kind = .native := h_native.2
  have h : restartDisposition row =
      .terminalize .terminalizeBackgroundedAsInterrupted := by
    simp [restartDisposition, h_native, h_session, h_live, h_deadline, h_process]
  refine ⟨h, ?_, ?_⟩
  · rw [h]; rfl
  · simp [RestartRow.notification, h_native, h_live, h]

theorem native_background_unstopped_process_settles_lost
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_owner : row.parent ≠ .missing)
    (h_process : row.process = .notOwned ∨ row.process = .alreadyExited) :
    restartDisposition row = .terminalize .processLost ∧
      (restartDisposition row).terminalStateContract = some "failed" ∧
      row.notification =
        some (restartNotificationObligation .processLost) := by
  have h_not_running : row.process ≠ .stillRunning := by
    rcases h_process with h | h <;> simp [h]
  have h_not_stopped : row.process ≠ .stopped := by
    rcases h_process with h | h <;> simp [h]
  have h_session : row.kind = .native := h_native.2
  have h : restartDisposition row = .terminalize .processLost := by
    simp [restartDisposition, h_owner, h_native, h_session, h_not_running,
      h_not_stopped]
  refine ⟨h, ?_, ?_⟩
  · rw [h]; rfl
  · simp [RestartRow.notification, h_native, h_owner, h]

theorem native_background_still_running_left_running
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_process : row.process = .stillRunning) :
    restartDisposition row = .leaveRunning := by
  by_cases h_owner : row.parent = .missing
  · simp [restartDisposition, h_owner]
  · simp [restartDisposition, h_owner, h_native, h_process]

theorem session_message_row_left_running
    (row : RestartRow)
    (h_session : row.kind = .sessionMessage) :
    restartDisposition row = .leaveRunning := by
  by_cases h_owner : row.parent = .missing
  · simp [restartDisposition, h_owner]
  · simp [restartDisposition, h_owner, h_session]

theorem plugin_job_row_left_running
    (row : RestartRow)
    (h_job : row.kind = .pluginJob) :
    restartDisposition row = .leaveRunning := by
  by_cases h_owner : row.parent = .missing
  · simp [restartDisposition, h_owner]
  · simp [restartDisposition, h_owner, h_job]

theorem restart_interrupt_iff_native_background_resolvable_parent
    (row : RestartRow) :
    restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted ↔
      (row.isNativeBackgroundTool ∧ row.parent ≠ .missing ∧
        row.deadlineExpired = false ∧ row.process = .stopped) := by
  rcases row with ⟨awaitMode, kind, parent, deadlineExpired, process⟩
  cases awaitMode <;> cases kind <;> cases parent <;>
    cases deadlineExpired <;> cases process <;> decide

theorem leave_running_iff_preserved_shapes (row : RestartRow) :
    restartDisposition row = .leaveRunning ↔
      (row.parent = .missing ∨ row.kind ≠ .native ∨
        (row.isNativeBackgroundTool ∧ row.process = .stillRunning) ∨
        (row.deadlineExpired = false ∧ ¬ row.isNativeBackgroundTool ∧
          row.parent = .live)) := by
  rcases row with ⟨awaitMode, kind, parent, deadlineExpired, process⟩
  cases awaitMode <;> cases kind <;> cases parent <;>
    cases deadlineExpired <;> cases process <;> decide

theorem terminalize_lands_terminal
    (row : RestartRow) (cause : ToolRecoveryCause)
    (_h : restartDisposition row = .terminalize cause) :
    isTerminal cause.terminalState :=
  cause.terminalState_terminal

theorem notification_iff_terminalized_native_background (row : RestartRow) :
    row.notification.isSome = true ↔
      (row.isNativeBackgroundTool ∧ row.parent ≠ .missing ∧
        restartDisposition row ≠ .leaveRunning) := by
  rcases row with ⟨awaitMode, kind, parent, deadlineExpired, process⟩
  cases awaitMode <;> cases kind <;> cases parent <;>
    cases deadlineExpired <;> cases process <;> decide

theorem deadline_precedes_restart_interrupt
    (row : RestartRow)
    (h_session : row.kind = .native)
    (h_owner : row.parent ≠ .missing)
    (h_process : row.process = .stopped)
    (h_expired : row.deadlineExpired = true) :
    restartDisposition row = .terminalize .deadlineExceeded := by
  simp [restartDisposition, h_session, h_owner, h_process, h_expired]

end Recovery
