import Proofs.Recovery.Sweeps.ToolCalls

/-!
# Startup Restart Disposition for Running Tool Rows (#937)

`ToolCallLifecycle::recover_all` does not terminalize every running row it
loads: the startup classifier in `recover_stuck_running_tool_calls`
(`tool_call_lifecycle/recovery.rs`) decides, per row, between a terminal
recovery cause and **leaving the row running**. This module is the total,
executable model of that classifier:

* **native background tool** (`await_mode = background`, a host process) with a
  resolvable parent → the host owner first stops a proven-owned process
  (`ManagedExec.stopOutcome`). A process still observed running keeps its row
  running; one the owner did not observe stopping (unrecorded, pid reused, or
  already exited) settles as `processLost`, never as an interruption. After an
  observed stop, every terminal restart disposition carries a durable
  completion notification and coalesced background-completion wake; the
  reason distinguishes restart interruption and deadline expiry, never the
  parent's state;
* **session-message row** (`create_session`/`send_message`) → **leave running**
  under every resolvable parent unless its own deadline expired. No host
  process backs it: the started session is an ordinary agent's session, and
  its caused request's terminal later terminalizes the row through the
  completion observer. A parent's fate is never a cancel signal for it;
* an unresolved exact physical parent defers all terminalization, including
  deadline expiry;
* deadline expiry takes precedence for resolvable parents;
* other rows under interrupted / otherwise-terminal parents terminalize as
  `parentInterrupted` / `parentTerminal`.

Scope notes, matching Rust:

* Caused-request terminal precedence runs *before* this classifier and is
  covered by `sessionMessageRecoverySweep`; rows reaching this classifier have
  no durable caused-request terminal yet.
* The classifier observes the parent as loaded at startup, before
  `RequestLifecycle::recover_all` runs (tool recovery is wired first in
  `agent/runtime/startup.rs`), so an orphaned `processing` parent still
  observes as live here; the request sweep and the periodic
  `terminalParentOwnedToolSweep` (#837) close that loop afterwards.
-/

namespace Recovery

open ToolExecution

/-- Parent request as observed by the startup classifier. `missing` covers
    rows whose exact physical request document cannot be resolved in the
    recovering principal's scope. Replication may supply that owner later;
    absent owner facts are never grounds for a local write. -/
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

/-- Terminal parent observations. `missing` is not terminal: an unresolvable
    parent never grounds a local terminalization. -/
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

/-- One running `AgentToolCall` row as the startup classifier sees it. The
    sweep scope is `lifecycle_state = "running"`, so the state itself is not
    a field. `sessionMessage` is the `create_session`/`send_message` tool kind. -/
structure RestartRow where
  awaitMode : AwaitMode
  sessionMessage : Bool
  parent : ParentObservation
  deadlineExpired : Bool
  /-- Host stop verdict for a native background process; other rows carry
      `stopped` and ignore it. -/
  process : ManagedExec.StopOutcome
  deriving DecidableEq, Repr

/-- What startup recovery does with one running row. -/
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

/-- A native background tool row: a background row backed by a host process. -/
def RestartRow.isNativeBackgroundTool (row : RestartRow) : Prop :=
  row.awaitMode = .background ∧ row.sessionMessage = false

instance (row : RestartRow) : Decidable row.isNativeBackgroundTool := by
  unfold RestartRow.isNativeBackgroundTool
  infer_instance

/-- Total startup disposition. Branch order is the production order in
    `recover_stuck_running_tool_calls`; reordering any two branches changes
    the value on some row and fails the exhaustive theorems below. -/
def restartDisposition (row : RestartRow) : RestartDisposition :=
  if row.parent = .missing then
    .leaveRunning
  else if row.isNativeBackgroundTool ∧ row.process = .stillRunning then
    .leaveRunning
  else if row.isNativeBackgroundTool ∧ row.process ≠ .stopped then
    .terminalize .processLost
  else if row.deadlineExpired then
    .terminalize .deadlineExceeded
  else if row.isNativeBackgroundTool then
    .terminalize .terminalizeBackgroundedAsInterrupted
  else if row.sessionMessage then
    .leaveRunning
  else if row.parent = .interrupted then
    .terminalize .parentInterrupted
  else if row.parent.observedTerminal then
    .terminalize .parentTerminal
  else
    .leaveRunning

/-- Project the periodic orphan observation onto the startup classifier's
    parent vocabulary. The boolean priority matches
    `orphanedBackgroundToolCause`. -/
def OrphanedBackgroundToolRow.parentObservation
    (row : OrphanedBackgroundToolRow) : ParentObservation :=
  if row.parentLive then .live
  else if row.parentInterrupted then .interrupted
  else if row.parentTerminal then .otherTerminal
  else .missing

def OrphanedBackgroundToolRow.toRestartRow
    (row : OrphanedBackgroundToolRow) : RestartRow :=
  { awaitMode := row.call.awaitMode
  , sessionMessage := decide (row.call.operation = .sessionMessage)
  , parent := row.parentObservation
  , deadlineExpired := row.deadlineExpired
  , process := row.process
  }

/-- The periodic orphan classifier is the native-background restriction of
    the startup classifier, including cause precedence and the host stop
    verdict. Task deletion and live workers are periodic-only inputs. -/
theorem orphanedBackgroundToolCause_matches_restartDisposition
    (row : OrphanedBackgroundToolRow)
    (h_background : row.call.awaitMode = .background)
    (h_native : row.call.operation ≠ .sessionMessage)
    (h_unregistered : row.executionRegistered = false)
    (h_task : row.ownerTaskDeleted = false) :
    (orphanedBackgroundToolCause row).map ToolRecoveryCause.toContract =
      (restartDisposition row.toRestartRow).causeContract := by
  cases h_deadline : row.deadlineExpired <;>
    cases h_live : row.parentLive <;>
    cases h_interrupted : row.parentInterrupted <;>
    cases h_terminal : row.parentTerminal <;>
    cases h_process : row.process <;>
    simp [orphanedBackgroundToolCause, OrphanedBackgroundToolRow.toRestartRow,
      OrphanedBackgroundToolRow.parentObservation,
      OrphanedBackgroundToolRow.parentResolvable, restartDisposition,
      RestartRow.isNativeBackgroundTool,
      RestartDisposition.causeContract, h_background, h_native, h_deadline,
      h_live, h_interrupted, h_terminal, h_process,
      h_unregistered, h_task, ParentObservation.observedTerminal]

/-- Durable side effects owed after terminalizing a native background tool on
    restart: the cause-specific `<tool-completion>` notification reason and
    the coalesced wake queue vocabulary
    (`background_completion:<parent session>`). -/
structure RestartNotificationObligation where
  notificationReason : String
  queueSource : String
  queueKeyPrefix : String
  deriving DecidableEq, Repr

def restartNotificationObligation
    (cause : ToolRecoveryCause) : RestartNotificationObligation :=
  { notificationReason :=
      match cause with
      | .deadlineExceeded => "deadline_exceeded"
      | .parentInterrupted => "parent_interrupted"
      | .parentTerminal => "parent_terminal"
      | .terminalizeBackgroundedAsInterrupted => "interrupted_on_restart"
      | .processLost => "process_lost"
      | .taskDeleted => "task_deleted"
  , queueSource := "background_completion"
  , queueKeyPrefix := "background_completion:"
  }

/-- A terminalized native background process with a resolvable parent always
    owes a notification + wake. This includes the normal cross-turn shape:
    the spawning request completed before the process restart. -/
def RestartRow.notification (row : RestartRow) :
    Option RestartNotificationObligation :=
  if row.isNativeBackgroundTool ∧ row.parent ≠ .missing then
    match restartDisposition row with
    | .terminalize cause => some (restartNotificationObligation cause)
    | _ => none
  else
    none

/-- A missing physical owner is incomplete observation even when a timer has
    elapsed. No terminal cause or notification can be published from it. -/
theorem missing_parent_never_terminalizes (row : RestartRow)
    (h : row.parent = .missing) :
    restartDisposition row = .leaveRunning ∧ row.notification = none := by
  constructor
  · simp [restartDisposition, h]
  · simp [RestartRow.notification, h]

/-! ## Pointwise theorems -/

/-- An observed stop of a lost native background process is attributed to
    the restart under every resolvable parent, never to the parent. -/
theorem native_background_tool_interrupted_on_restart
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_owner : row.parent ≠ .missing)
    (h_deadline : row.deadlineExpired = false)
    (h_process : row.process = .stopped) :
    restartDisposition row =
      .terminalize .terminalizeBackgroundedAsInterrupted := by
  simp [restartDisposition, h_native, h_owner, h_deadline, h_process]

/-- RB1: a native background tool with a live parent, no expiry, and an
    observed stop of its proven-owned process is interrupted on restart —
    terminal `cancelled` plus the durable notification/wake obligation. -/
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
  have h : restartDisposition row =
      .terminalize .terminalizeBackgroundedAsInterrupted := by
    simp [restartDisposition, h_native, h_live, h_deadline, h_process]
  refine ⟨h, ?_, ?_⟩
  · rw [h]; rfl
  · simp [RestartRow.notification, h_native, h_live, h]

/-- RB1′: a native background process the owner did not observe stopping
    is settled as lost — terminal `failed` with its own notification — rather
    than reported as interrupted. Precedes expiry and parent-state causes. -/
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
  have h : restartDisposition row = .terminalize .processLost := by
    simp [restartDisposition, h_owner, h_native, h_not_running, h_not_stopped]
  refine ⟨h, ?_, ?_⟩
  · rw [h]; rfl
  · simp [RestartRow.notification, h_native, h_owner, h]

/-- RB1″: a native background process still observed running after the
    owner's signal keeps its row running; no terminal state is published for
    work that may still produce effects. -/
theorem native_background_still_running_left_running
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_process : row.process = .stillRunning) :
    restartDisposition row = .leaveRunning := by
  by_cases h_owner : row.parent = .missing
  · simp [restartDisposition, h_owner]
  · simp [restartDisposition, h_owner, h_native, h_process]

/-- RB2: a session-message row is left running under every resolvable parent
    without expiry — live, interrupted or terminal. The started session is
    addressed directly and owns its own lifetime; its caused request's
    terminal later closes the row through the completion observer. -/
theorem session_message_row_left_running
    (row : RestartRow)
    (h_session : row.sessionMessage = true)
    (h_deadline : row.deadlineExpired = false) :
    restartDisposition row = .leaveRunning := by
  have h_not_native : ¬ row.isNativeBackgroundTool := by
    simp [RestartRow.isNativeBackgroundTool, h_session]
  by_cases h_owner : row.parent = .missing
  · simp [restartDisposition, h_owner]
  · simp [restartDisposition, h_owner, h_not_native, h_deadline, h_session]

/-- No parent observation ever terminalizes a session-message row: only its
    own deadline does. -/
theorem session_message_row_terminalizes_only_on_expiry
    (row : RestartRow) (cause : ToolRecoveryCause)
    (h_session : row.sessionMessage = true)
    (h : restartDisposition row = .terminalize cause) :
    cause = .deadlineExceeded := by
  rcases row with ⟨awaitMode, sessionMessage, parent, deadlineExpired, process⟩
  simp only at h_session
  subst h_session
  cases awaitMode <;> cases parent <;> cases deadlineExpired <;> cases process <;>
    simp [restartDisposition, RestartRow.isNativeBackgroundTool,
      ParentObservation.observedTerminal] at h <;> (subst h; rfl)

/-! ## Exhaustive characterizations

Every field of `RestartRow` is finite, so these are proved by exhausting the
full 160-row input space (2 await modes × 2 tool kinds × 5 parent observations
× 2 deadline flags × 4 stop outcomes) — they are complete characterizations of
the classifier, not spot checks. -/

/-- The restart interrupt fires **exactly** for a native background tool with
    a resolvable parent, no expiry and an observed stop — never for a
    session-message row, never over deadline precedence. -/
theorem restart_interrupt_iff_native_background_resolvable_parent
    (row : RestartRow) :
    restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted ↔
      (row.isNativeBackgroundTool ∧ row.parent ≠ .missing ∧
        row.deadlineExpired = false ∧ row.process = .stopped) := by
  rcases row with ⟨awaitMode, sessionMessage, parent, deadlineExpired, process⟩
  cases awaitMode <;> cases sessionMessage <;> cases parent <;>
    cases deadlineExpired <;> cases process <;> decide

/-- Leave-running fires exactly on the preserved shapes: a missing parent
    regardless of expiry, a native background process still observed running,
    and, without expiry, a session-message row or a non-native row under a
    live parent. -/
theorem leave_running_iff_preserved_shapes (row : RestartRow) :
    restartDisposition row = .leaveRunning ↔
      (row.parent = .missing ∨
        (row.isNativeBackgroundTool ∧ row.process = .stillRunning) ∨
        (row.deadlineExpired = false ∧ ¬ row.isNativeBackgroundTool ∧
          (row.sessionMessage = true ∨ row.parent = .live))) := by
  rcases row with ⟨awaitMode, sessionMessage, parent, deadlineExpired, process⟩
  cases awaitMode <;> cases sessionMessage <;> cases parent <;>
    cases deadlineExpired <;> cases process <;> decide

/-- Every terminalized row lands on a terminal tool-call state (feeds
    `toolCallRecoverySweep`'s convergence contract). -/
theorem terminalize_lands_terminal
    (row : RestartRow) (cause : ToolRecoveryCause)
    (_h : restartDisposition row = .terminalize cause) :
    isTerminal cause.terminalState :=
  cause.terminalState_terminal

/-- Notification is owed exactly when a resolvable native background process
    is terminalized. Session-message rows owe nothing here; their
    notification comes from the completion observer later. -/
theorem notification_iff_terminalized_native_background (row : RestartRow) :
    row.notification.isSome = true ↔
      (row.isNativeBackgroundTool ∧ row.parent ≠ .missing ∧
        restartDisposition row ≠ .leaveRunning) := by
  rcases row with ⟨awaitMode, sessionMessage, parent, deadlineExpired, process⟩
  cases awaitMode <;> cases sessionMessage <;> cases parent <;>
    cases deadlineExpired <;> cases process <;> decide

/-- Deadline expiry outranks the restart interrupt: an expired native
    background tool times out (external failure) instead of reading as an
    operator interrupt. -/
theorem deadline_precedes_restart_interrupt
    (row : RestartRow)
    (h_owner : row.parent ≠ .missing)
    (h_process : row.process = .stopped)
    (h_expired : row.deadlineExpired = true) :
    restartDisposition row = .terminalize .deadlineExceeded := by
  simp [restartDisposition, h_owner, h_process, h_expired]

end Recovery
