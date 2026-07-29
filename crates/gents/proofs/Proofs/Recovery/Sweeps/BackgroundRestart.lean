import Proofs.Recovery.Sweeps.ToolCalls

/-!
# Startup Restart Disposition for Running Tool Rows (#937)

`ToolCallLifecycle::recover_all` does not terminalize every running row it
loads: the startup classifier in `recover_stuck_running_tool_calls`
(`tool_call_lifecycle/recovery.rs`) decides, per row, between a terminal
recovery cause and **leaving the row running**. The existing
`toolCallRecoverySweep` contract only models rows that already carry a cause;
the classifier itself — in particular the two leave-running arms and the
kind split between background *subagents* and native background *tools* —
was previously unmodeled, so a "model-driven" closeout could have
terminalized rows production deliberately preserves.

This module is the total, executable model of that classifier:

* **native background tool** (`await_mode = background`, no child request)
  with a live parent → terminalize as
  `TerminalizeBackgroundedAsInterrupted` (→ `cancelled`), with a durable
  completion notification (`interrupted_on_restart`) and a coalesced
  background-completion wake obligation — the in-memory execution and its
  live output ring buffer died with the process;
* **background subagent bridge** (`await_mode = background`, child request
  linked) with a live parent → **leave running** — the durable bridge row is
  the work, and the child terminal projects later;
* detached bridge under an interrupted parent → leave running;
* child-linked bridge under a cleanly completed parent → leave running;
* deadline / unclaimed-spawn expiry take precedence over everything;
* interrupted / otherwise-terminal parents terminalize as
  `parentInterrupted` / `parentTerminal`.

Scope notes, matching Rust:

* Child-terminal precedence (`recover_bridge_terminal_child`) runs *before*
  this classifier and is covered by the `childCompleted`/`childFailed`/… rows
  of `toolCallRecoverySweep`; rows reaching this classifier have no durable
  child terminal yet.
* The classifier observes the parent as loaded at startup, before
  `RequestLifecycle::recover_all` runs (tool recovery is wired first in
  `agent/runtime/startup.rs`), so an orphaned `processing` parent still
  observes as live here; the request sweep and the periodic
  `terminalParentOwnedToolSweep` (#837) close that loop afterwards.
-/

namespace Recovery

open ToolExecution

/-- Parent request as observed by the startup classifier. `missing` covers
    rows whose `request_id` resolves to no row under the recovering agent's
    DID (foreign or deleted parents are never grounds for a local write). -/
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
    a field. `childLinked` is `child_request_id` non-empty: `true` makes the
    row a subagent bridge, `false` a native background-bridge or plain tool
    row. -/
structure RestartRow where
  awaitMode : Subagent.AwaitMode
  cancelPolicy : Subagent.CancelPolicy
  childLinked : Bool
  parent : ParentObservation
  deadlineExpired : Bool
  unclaimedExpired : Bool
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
  | .leaveRunning => none

def terminalStateContract : RestartDisposition → Option String
  | .terminalize cause => some cause.terminalState.toDefraDB
  | .leaveRunning => none

end RestartDisposition

/-- A native background tool row: R6 bridge row with no child request. -/
def RestartRow.isNativeBackgroundTool (row : RestartRow) : Prop :=
  row.awaitMode = .background ∧ row.childLinked = false

instance (row : RestartRow) : Decidable row.isNativeBackgroundTool := by
  unfold RestartRow.isNativeBackgroundTool
  infer_instance

/-- A background subagent bridge row: R5 bridge row with a child request. -/
def RestartRow.isBackgroundSubagentBridge (row : RestartRow) : Prop :=
  row.awaitMode = .background ∧ row.childLinked = true

instance (row : RestartRow) : Decidable row.isBackgroundSubagentBridge := by
  unfold RestartRow.isBackgroundSubagentBridge
  infer_instance

/-- A detached bridge: child-linked with `cancel_policy = detach`. -/
def RestartRow.isDetachedBridge (row : RestartRow) : Prop :=
  row.childLinked = true ∧ row.cancelPolicy = .detach

instance (row : RestartRow) : Decidable row.isDetachedBridge := by
  unfold RestartRow.isDetachedBridge
  infer_instance

/-- Total startup disposition. Branch order is the production order in
    `recover_stuck_running_tool_calls`; reordering any two branches changes
    the value on some row and fails the exhaustive theorems below. -/
def restartDisposition (row : RestartRow) : RestartDisposition :=
  if row.deadlineExpired then
    .terminalize .deadlineExceeded
  else if row.unclaimedExpired then
    .terminalize .unclaimedCrossDeploymentSpawn
  else if row.isNativeBackgroundTool ∧ row.parent = .live then
    .terminalize .terminalizeBackgroundedAsInterrupted
  else if row.isDetachedBridge ∧ row.parent = .interrupted then
    .leaveRunning
  else if row.parent = .cleanlyCompleted ∧ row.childLinked then
    .leaveRunning
  else if row.parent = .interrupted then
    .terminalize .parentInterrupted
  else if row.parent.observedTerminal then
    .terminalize .parentTerminal
  else
    .leaveRunning

/-- Durable side effects owed after terminalizing a native background tool on
    restart: the `<tool-completion status="cancelled">` notification reason
    and the coalesced wake queue vocabulary
    (`background_completion:<parent session>`). -/
structure RestartNotificationObligation where
  notificationReason : String
  queueSource : String
  queueKeyPrefix : String
  deriving DecidableEq, Repr

def restartNotificationObligation : RestartNotificationObligation :=
  { notificationReason := "interrupted_on_restart"
  , queueSource := "background_completion"
  , queueKeyPrefix := "background_completion:"
  }

/-- The notification + wake are owed exactly on the restart-interrupt arm. -/
def RestartDisposition.notification :
    RestartDisposition → Option RestartNotificationObligation
  | .terminalize .terminalizeBackgroundedAsInterrupted =>
      some restartNotificationObligation
  | _ => none

/-! ## Pointwise theorems (the four #937 arms) -/

/-- RB1: a native background tool with a live parent and no expiry is
    interrupted on restart — terminal `cancelled` plus the durable
    notification/wake obligation. -/
theorem native_background_tool_live_parent_interrupted_on_restart
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_live : row.parent = .live)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false) :
    restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted ∧
      (restartDisposition row).terminalStateContract = some "cancelled" ∧
      (restartDisposition row).notification =
        some restartNotificationObligation := by
  have h : restartDisposition row =
      .terminalize .terminalizeBackgroundedAsInterrupted := by
    simp [restartDisposition, h_native, h_live, h_deadline, h_unclaimed]
  refine ⟨h, ?_, ?_⟩
  · rw [h]; rfl
  · rw [h]; rfl

/-- RB2: a background subagent bridge with a live parent is left running on
    restart — the durable bridge row survives the process and projects the
    child terminal later. -/
theorem background_subagent_bridge_live_parent_left_running
    (row : RestartRow)
    (h_bridge : row.isBackgroundSubagentBridge)
    (h_live : row.parent = .live)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false) :
    restartDisposition row = .leaveRunning := by
  have h_child : row.childLinked = true := h_bridge.2
  have h_not_native : ¬ row.isNativeBackgroundTool := by
    simp [RestartRow.isNativeBackgroundTool, h_child]
  simp [restartDisposition, h_not_native, h_live, h_deadline, h_unclaimed,
    ParentObservation.observedTerminal]

/-- RB3: a detached bridge under an interrupted parent is left running —
    detach means the child (and its bridge) outlive the parent's interrupt. -/
theorem detached_bridge_interrupted_parent_left_running
    (row : RestartRow)
    (h_detached : row.isDetachedBridge)
    (h_parent : row.parent = .interrupted)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false) :
    restartDisposition row = .leaveRunning := by
  have h_child : row.childLinked = true := h_detached.1
  have h_not_native : ¬ row.isNativeBackgroundTool := by
    simp [RestartRow.isNativeBackgroundTool, h_child]
  simp [restartDisposition, h_not_native, h_detached, h_parent, h_deadline,
    h_unclaimed]

/-- RB4: clean parent completion is not a cancel signal for a child-linked
    bridge — the row is left running. -/
theorem clean_completion_child_linked_left_running
    (row : RestartRow)
    (h_child : row.childLinked = true)
    (h_parent : row.parent = .cleanlyCompleted)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false) :
    restartDisposition row = .leaveRunning := by
  have h_not_native : ¬ row.isNativeBackgroundTool := by
    simp [RestartRow.isNativeBackgroundTool, h_child]
  simp [restartDisposition, h_not_native, h_parent, h_child, h_deadline,
    h_unclaimed]

/-! ## Exhaustive characterizations

Every field of `RestartRow` is finite, so these are proved by exhausting the
full 160-row input space (2 await modes × 2 cancel policies × 2 child links ×
5 parent observations × 2 deadline flags × 2 unclaimed flags) — they are
complete characterizations of the classifier, not spot checks. -/

/-- The restart interrupt fires **exactly** for a native background tool with
    a live parent and no expiry — never for a subagent bridge, never under a
    terminal parent, never over deadline/unclaimed precedence. -/
theorem restart_interrupt_iff_native_background_live_parent
    (row : RestartRow) :
    restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted ↔
      (row.isNativeBackgroundTool ∧ row.parent = .live ∧
        row.deadlineExpired = false ∧ row.unclaimedExpired = false) := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired⟩
  cases awaitMode <;> cases cancelPolicy <;> cases childLinked <;>
    cases parent <;> cases deadlineExpired <;> cases unclaimedExpired <;>
    decide

/-- Leave-running fires exactly on the four preserved shapes (with no
    expiry): a missing parent (never grounds a local write, even for a native
    background tool), a live parent without the native-background-interrupt
    shape, a detached bridge under an interrupted parent, and a child-linked
    bridge under a cleanly completed parent. -/
theorem leave_running_iff_preserved_shapes (row : RestartRow) :
    restartDisposition row = .leaveRunning ↔
      (row.deadlineExpired = false ∧ row.unclaimedExpired = false ∧
        (row.parent = .missing ∨
          (row.parent = .live ∧ ¬ row.isNativeBackgroundTool) ∨
          (row.isDetachedBridge ∧ row.parent = .interrupted) ∨
          (row.childLinked = true ∧ row.parent = .cleanlyCompleted))) := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired⟩
  cases awaitMode <;> cases cancelPolicy <;> cases childLinked <;>
    cases parent <;> cases deadlineExpired <;> cases unclaimedExpired <;>
    decide

/-- Every terminalized row lands on a terminal tool-call state (feeds
    `toolCallRecoverySweep`'s convergence contract). -/
theorem terminalize_lands_terminal
    (row : RestartRow) (cause : ToolRecoveryCause)
    (_h : restartDisposition row = .terminalize cause) :
    isTerminal cause.terminalState :=
  cause.terminalState_terminal

/-- The notification + coalesced wake obligation is owed exactly on the
    restart-interrupt arm — subagent bridges left running owe nothing at
    restart (their notification comes from completion projection later). -/
theorem notification_iff_restart_interrupt (row : RestartRow) :
    (restartDisposition row).notification =
        some restartNotificationObligation ↔
      restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired⟩
  cases awaitMode <;> cases cancelPolicy <;> cases childLinked <;>
    cases parent <;> cases deadlineExpired <;> cases unclaimedExpired <;>
    decide

/-- Deadline expiry outranks the restart interrupt: an expired native
    background tool times out (external failure) instead of reading as an
    operator interrupt. -/
theorem deadline_precedes_restart_interrupt
    (row : RestartRow)
    (h_expired : row.deadlineExpired = true) :
    restartDisposition row = .terminalize .deadlineExceeded := by
  simp [restartDisposition, h_expired]

/-- Unclaimed-spawn expiry outranks every leave-running exemption: a bridge
    whose spawn was never claimed fails even under a live parent. -/
theorem unclaimed_precedes_leave_running_exemptions
    (row : RestartRow)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = true) :
    restartDisposition row = .terminalize .unclaimedCrossDeploymentSpawn := by
  simp [restartDisposition, h_deadline, h_unclaimed]

end Recovery
