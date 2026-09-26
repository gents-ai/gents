import Proofs.Recovery.Sweeps.ToolCalls
import Proofs.SpawnClaimFence

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
  with a resolvable parent → the host owner first stops a proven-owned
  process (`ManagedExec.stopOutcome`). A process still observed running keeps
  its row running; one the owner did not observe stopping (unrecorded, pid
  reused, or already exited) settles as `processLost`, never as an
  interruption. After an observed stop, every terminal restart disposition
  carries a durable completion notification and coalesced
  background-completion wake; the reason distinguishes restart interruption
  and deadline expiry, never the parent's state: a lost process is attributed
  to the restart, whatever the parent's state. Canonical output already
  committed to durable records remains available;
* **background subagent bridge** (`await_mode = background`, child request
  linked) with a live parent → **leave running** — the durable bridge row is
  the work, and the child terminal projects later;
* child-linked bridge under any terminal parent → **retain in background**,
  whatever its cancellation policy: a parent's fate is never a cancel signal
  for a subagent. An awaited bridge becomes background work with its one
  immutable invocation receipt (the `Subagent.Interrupt` background
  disposition), so the child's terminal is later delivered as a completion
  notification; an already-background bridge only has its receipt ensured;
* an unresolved exact physical parent defers all terminalization, including
  deadline / unclaimed-spawn expiry;
* deadline / unclaimed-spawn expiry take precedence for resolvable parents;
* other rows under interrupted / otherwise-terminal parents terminalize as
  `parentInterrupted` / `parentTerminal`.

The request's own terminal accounting applies the same retention inside its
terminal transaction, so a crash between the interrupt latch and the live
hook's retention is repaired when request recovery terminalizes the parent;
this startup arm covers rows whose parent was already terminal.

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
  /-- Host stop verdict for a native background process; other rows carry
      `stopped` and ignore it. -/
  process : ManagedExec.StopOutcome
  /-- A child row corroborating this bridge's exact physical lineage and target
      principal is visible to the recovering parent (#1807). -/
  childObserved : Bool := false
  deriving DecidableEq, Repr

/-- What startup recovery does with one running row. -/
inductive RestartDisposition where
  | terminalize (cause : ToolRecoveryCause)
  | leaveRunning
  /-- Unclaimed expiry observed the child: the deadline clears and the bridge
      keeps running (`SpawnClaimFence.expire`). -/
  | link
  /-- Keep the child-linked bridge running as background work: flip an
      awaited bridge to background and ensure its invocation receipt in the
      same transaction. A failed receipt write must leave the mode unchanged. -/
  | retainInBackground
  deriving DecidableEq, Repr

namespace RestartDisposition

def toContract : RestartDisposition → String
  | .terminalize _ => "terminalize"
  | .leaveRunning => "leave_running"
  | .link => "link"
  | .retainInBackground => "retain_in_background"

def causeContract : RestartDisposition → Option String
  | .terminalize cause => some cause.toContract
  | _ => none

def terminalStateContract : RestartDisposition → Option String
  | .terminalize cause => some cause.terminalState.toDefraDB
  | _ => none

/-- Await mode after the disposition, when the disposition sets one. -/
def postAwaitModeContract : RestartDisposition → Option String
  | .retainInBackground => some Subagent.AwaitMode.background.toDefraDB
  | _ => none

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

/-- The spawn fence's view of a child-linked row. -/
def RestartRow.fenceWorld (row : RestartRow) : SpawnClaimFence.World :=
  { SpawnClaimFence.World.initial with childVisible := row.childLinked && row.childObserved }

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
  else if row.unclaimedExpired then
    match (SpawnClaimFence.expire row.fenceWorld).bridge with
    | .linked => .link
    | _ => .terminalize .unclaimedCrossPrincipalSpawn
  else if row.isNativeBackgroundTool then
    .terminalize .terminalizeBackgroundedAsInterrupted
  else if row.childLinked ∧ row.parent.observedTerminal then
    .retainInBackground
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
  , cancelPolicy := row.call.cancelPolicy
  , childLinked := row.call.childRequestId.isSome
  , parent := row.parentObservation
  , deadlineExpired := row.deadlineExpired
  , unclaimedExpired := row.unclaimedExpired
  , process := row.process
  , childObserved := false
  }

/-- The periodic orphan classifier is the native-background restriction of
    the startup classifier, including cause precedence and the host stop
    verdict. Task deletion and live workers are periodic-only inputs. -/
theorem orphanedBackgroundToolCause_matches_restartDisposition
    (row : OrphanedBackgroundToolRow)
    (h_background : row.call.awaitMode = .background)
    (h_native : row.call.childRequestId = none)
    (h_unregistered : row.executionRegistered = false)
    (h_task : row.ownerTaskDeleted = false) :
    (orphanedBackgroundToolCause row).map ToolRecoveryCause.toContract =
      (restartDisposition row.toRestartRow).causeContract := by
  cases h_deadline : row.deadlineExpired <;>
    cases h_unclaimed : row.unclaimedExpired <;>
    cases h_live : row.parentLive <;>
    cases h_interrupted : row.parentInterrupted <;>
    cases h_terminal : row.parentTerminal <;>
    cases h_process : row.process <;>
    simp [orphanedBackgroundToolCause, OrphanedBackgroundToolRow.toRestartRow,
      OrphanedBackgroundToolRow.parentObservation,
      OrphanedBackgroundToolRow.parentResolvable, restartDisposition,
      RestartRow.isNativeBackgroundTool,
      RestartDisposition.causeContract, h_background, h_native, h_deadline,
      h_unclaimed, h_live, h_interrupted, h_terminal, h_process,
      h_unregistered, h_task, ParentObservation.observedTerminal, RestartRow.fenceWorld,
      SpawnClaimFence.expire, SpawnClaimFence.fence, SpawnClaimFence.World.initial]

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
      | .unclaimedCrossPrincipalSpawn => "unclaimed_spawn_timeout"
      | .processLost => "process_lost"
      | .taskDeleted => "task_deleted"
      | _ => "tool_failed"
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

/-! ## Pointwise theorems (the four #937 arms) -/

/-- The cancellation policy never affects a restart disposition. -/
theorem restartDisposition_ignores_cancel_policy
    (row : RestartRow) (policy : Subagent.CancelPolicy) :
    restartDisposition { row with cancelPolicy := policy } =
      restartDisposition row := by
  simp [restartDisposition, RestartRow.isNativeBackgroundTool, RestartRow.fenceWorld]

/-- An observed stop of a lost native background process is attributed to
    the restart under every resolvable parent, never to the parent. -/
theorem native_background_tool_interrupted_on_restart
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_owner : row.parent ≠ .missing)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false)
    (h_process : row.process = .stopped) :
    restartDisposition row =
      .terminalize .terminalizeBackgroundedAsInterrupted := by
  simp [restartDisposition, h_native, h_owner, h_deadline, h_unclaimed, h_process]

/-- RB1: a native background tool with a resolvable parent, no expiry, and an
    observed stop of its proven-owned process is interrupted on restart —
    terminal `cancelled` plus the durable notification/wake obligation. -/
theorem native_background_tool_resolvable_parent_interrupted_on_restart
    (row : RestartRow)
    (h_native : row.isNativeBackgroundTool)
    (h_owner : row.parent ≠ .missing)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false)
    (h_process : row.process = .stopped) :
    restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted ∧
      (restartDisposition row).terminalStateContract = some "cancelled" ∧
      row.notification =
        some (restartNotificationObligation
          .terminalizeBackgroundedAsInterrupted) := by
  have h : restartDisposition row =
      .terminalize .terminalizeBackgroundedAsInterrupted := by
    simp [restartDisposition, h_native, h_owner, h_deadline, h_unclaimed,
      h_process]
  refine ⟨h, ?_, ?_⟩
  · rw [h]; rfl
  · simp [RestartRow.notification, h_native, h_owner, h]

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

/-- RB3: a child-linked bridge under any terminal parent is retained in
    background, whatever its cancellation policy and await mode — neither
    interrupting, failing nor completing the parent is a cancel signal for its
    subagents, and an awaited bridge's child terminal must still be delivered
    as a completion notification. -/
theorem child_linked_terminal_parent_retained_in_background
    (row : RestartRow)
    (h_child : row.childLinked = true)
    (h_parent : row.parent.observedTerminal)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false) :
    restartDisposition row = .retainInBackground ∧
      (restartDisposition row).postAwaitModeContract = some "background" := by
  have h_not_native : ¬ row.isNativeBackgroundTool := by
    simp [RestartRow.isNativeBackgroundTool, h_child]
  have h_present : row.parent ≠ .missing := by
    intro h; simp [h, ParentObservation.observedTerminal] at h_parent
  have h : restartDisposition row = .retainInBackground := by
    simp [restartDisposition, h_not_native, h_child, h_parent, h_present,
      h_deadline, h_unclaimed]
  exact ⟨h, by rw [h]; rfl⟩

/-- A foreground tool without a linked child follows its interrupted parent
    once its own expiry fences are clear. -/
theorem foreground_unlinked_interrupted_parent_terminalizes
    (row : RestartRow)
    (h_foreground : row.awaitMode = .foreground)
    (h_child : row.childLinked = false)
    (h_parent : row.parent = .interrupted)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = false) :
    restartDisposition row = .terminalize .parentInterrupted := by
  simp [restartDisposition, h_foreground, h_child, h_parent, h_deadline,
    h_unclaimed, RestartRow.isNativeBackgroundTool]

/-- Whether a disposition terminalizes only on the row's own expiry. -/
def RestartDisposition.expiryOnly : RestartDisposition → Bool
  | .terminalize .deadlineExceeded => true
  | .terminalize .unclaimedCrossPrincipalSpawn => true
  | .terminalize _ => false
  | _ => true

/-- No parent observation ever terminalizes a child-linked bridge: only its
    own deadline or an unclaimed spawn does. -/
theorem child_linked_bridge_terminalizes_only_on_expiry
    (row : RestartRow) (cause : ToolRecoveryCause)
    (h_child : row.childLinked = true)
    (h : restartDisposition row = .terminalize cause) :
    cause = .deadlineExceeded ∨ cause = .unclaimedCrossPrincipalSpawn := by
  have h_only : (restartDisposition row).expiryOnly = true := by
    rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
      deadlineExpired, unclaimedExpired, process, childObserved⟩
    simp only at h_child
    subst h_child
    cases awaitMode <;> cases cancelPolicy <;> cases parent <;>
      cases deadlineExpired <;> cases unclaimedExpired <;> cases process <;>
      cases childObserved <;> decide
  rw [h] at h_only
  cases cause <;> simp_all [RestartDisposition.expiryOnly]

/-! ## Exhaustive characterizations

Every field of `RestartRow` is finite, so these are proved by exhausting the
full 640-row input space (2 await modes × 2 cancel policies × 2 child links ×
5 parent observations × 2 deadline flags × 2 unclaimed flags × 4 stop
outcomes) — they are
complete characterizations of the classifier, not spot checks. -/

/-- The restart interrupt fires **exactly** for a native background tool with
    a resolvable parent, no expiry and an observed stop — never for a subagent
    bridge, never over deadline/unclaimed precedence. -/
theorem restart_interrupt_iff_native_background_resolvable_parent
    (row : RestartRow) :
    restartDisposition row =
        .terminalize .terminalizeBackgroundedAsInterrupted ↔
      (row.isNativeBackgroundTool ∧ row.parent ≠ .missing ∧
        row.deadlineExpired = false ∧ row.unclaimedExpired = false ∧
        row.process = .stopped) := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired, process, childObserved⟩
  cases awaitMode <;> cases cancelPolicy <;> cases childLinked <;>
    cases parent <;> cases deadlineExpired <;> cases unclaimedExpired <;>
    cases process <;> cases childObserved <;> decide

/-- Leave-running fires exactly on the preserved shapes: a missing parent
    regardless of expiry, a native background process still observed running,
    and a live parent without the native-background shape and without
    expiry. -/
theorem leave_running_iff_preserved_shapes (row : RestartRow) :
    restartDisposition row = .leaveRunning ↔
      (row.parent = .missing ∨
        (row.isNativeBackgroundTool ∧ row.process = .stillRunning) ∨
        (row.deadlineExpired = false ∧ row.unclaimedExpired = false ∧
          row.parent = .live ∧ ¬ row.isNativeBackgroundTool)) := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired, process, childObserved⟩
  cases awaitMode <;> cases cancelPolicy <;> cases childLinked <;>
    cases parent <;> cases deadlineExpired <;> cases unclaimedExpired <;>
    cases process <;> cases childObserved <;> decide

/-- Retention fires exactly for a child-linked bridge under a terminal parent
    without expiry. -/
theorem retain_in_background_iff_child_linked_terminal_parent (row : RestartRow) :
    restartDisposition row = .retainInBackground ↔
      (row.childLinked = true ∧ row.parent.observedTerminal ∧
        row.deadlineExpired = false ∧ row.unclaimedExpired = false) := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired, process, childObserved⟩
  cases awaitMode <;> cases cancelPolicy <;> cases childLinked <;>
    cases parent <;> cases deadlineExpired <;> cases unclaimedExpired <;>
    cases process <;> cases childObserved <;> decide

/-- Every terminalized row lands on a terminal tool-call state (feeds
    `toolCallRecoverySweep`'s convergence contract). -/
theorem terminalize_lands_terminal
    (row : RestartRow) (cause : ToolRecoveryCause)
    (_h : restartDisposition row = .terminalize cause) :
    isTerminal cause.terminalState :=
  cause.terminalState_terminal

/-- Notification is owed exactly when a resolvable native background process
    is terminalized. Subagent bridges owe nothing here; their notification
    comes from child-completion projection later. -/
theorem notification_iff_terminalized_native_background (row : RestartRow) :
    row.notification.isSome = true ↔
      (row.isNativeBackgroundTool ∧ row.parent ≠ .missing ∧
        restartDisposition row ≠ .leaveRunning) := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired, process, childObserved⟩
  cases awaitMode <;> cases cancelPolicy <;> cases childLinked <;>
    cases parent <;> cases deadlineExpired <;> cases unclaimedExpired <;>
    cases process <;> cases childObserved <;> decide

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

/-- Unclaimed-spawn expiry outranks every leave-running exemption: a bridge
    whose spawn was never claimed fails even under a live parent. -/
theorem unclaimed_precedes_leave_running_exemptions
    (row : RestartRow)
    (h_owner : row.parent ≠ .missing)
    (h_process : row.process = .stopped)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = true)
    (h_unobserved : row.childObserved = false) :
    restartDisposition row = .terminalize .unclaimedCrossPrincipalSpawn := by
  simp [restartDisposition, h_owner, h_process, h_deadline, h_unclaimed, RestartRow.fenceWorld,
    h_unobserved, SpawnClaimFence.expire, SpawnClaimFence.fence,
    SpawnClaimFence.World.initial]

/-- Unclaimed expiry that observes the child links the bridge instead. -/
theorem unclaimed_observed_child_links
    (row : RestartRow)
    (h_owner : row.parent ≠ .missing)
    (h_deadline : row.deadlineExpired = false)
    (h_unclaimed : row.unclaimedExpired = true)
    (h_child : row.childLinked = true)
    (h_observed : row.childObserved = true) :
    restartDisposition row = .link := by
  have h_not_native : ¬ row.isNativeBackgroundTool := by
    simp [RestartRow.isNativeBackgroundTool, h_child]
  simp [restartDisposition, h_owner, h_deadline, h_unclaimed, RestartRow.fenceWorld,
    h_child, h_observed, h_not_native, SpawnClaimFence.expire, SpawnClaimFence.World.initial]

/-- The spawn fence a restart settlement of a child-linked row writes: the
    same `SpawnClaimFence` transition that the deadline which fired first
    selects. `none` when restart recovery does not settle the row by expiry. -/
def RestartRow.spawnFence (row : RestartRow) : Option SpawnClaimFence.World :=
  if row.childLinked ∧ row.parent ≠ .missing then
    if row.deadlineExpired then some (SpawnClaimFence.deadline row.fenceWorld)
    else if row.unclaimedExpired then some (SpawnClaimFence.expire row.fenceWorld)
    else none
  else none

/-- A restart expiry of an unobserved child-linked row always fences it. -/
theorem restart_expiry_fences_unobserved_child (row : RestartRow)
    (h_child : row.childLinked = true) (h_owner : row.parent ≠ .missing)
    (h_unobserved : row.childObserved = false)
    (h_expired : row.deadlineExpired = true ∨ row.unclaimedExpired = true) :
    ∃ w, row.spawnFence = some w ∧ w.cancelIntent = true ∧ w.ackPending = true := by
  rcases row with ⟨awaitMode, cancelPolicy, childLinked, parent,
    deadlineExpired, unclaimedExpired, process, childObserved⟩
  simp at h_child h_unobserved h_owner
  subst h_child h_unobserved
  cases deadlineExpired <;> cases unclaimedExpired <;> simp at h_expired <;>
    simp [RestartRow.spawnFence, RestartRow.fenceWorld, h_owner,
      SpawnClaimFence.deadline, SpawnClaimFence.expire, SpawnClaimFence.fence,
      SpawnClaimFence.World.initial]

end Recovery
