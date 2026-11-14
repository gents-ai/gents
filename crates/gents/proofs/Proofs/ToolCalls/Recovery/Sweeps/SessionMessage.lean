import Proofs.ToolCalls.Recovery.Sweeps.ToolCalls

namespace Recovery

open ToolExecution

/-! A session-message row settles on its caused request's terminal, or fails
closed when it cannot name that request (`ToolRecoveryCause.causedRequestUnbound`).
A caused request lives on the calling runtime (`observeCaused`), so its terminal
is read in that runtime's own store; no peer replication is awaited. -/

def sessionMessageRecoverySweep : RecoverySweep :=
  toolCallRecoverySweepFor .sessionMessage
    "tool_call_lifecycle_recover_session_message_rows"
    "background_completion::settle_running_session_message_rows" .periodic

/-- A plugin-job row's terminal is the job driver's decision: the
    session-message sweep never selects it. -/
theorem session_message_sweep_never_selects_plugin_job
    (row : ToolCallRecoveryRow) (h_job : row.call.operation = .pluginJob) :
    ¬ toolCallRecoveryStale .sessionMessage row := by
  simp [toolCallRecoveryStale, RowClass.of, h_job]

/-- What a kill reads about the request a session-message row (or a plugin-job
child) caused.

Decision (supported topology, 2026-11-07): P2P carries only discovery and
enrollment. Session messages and plugin-job children cause requests only on
the calling runtime; cross-runtime delivery is refused at planning
(`session::message::plan`) and at staging, and an Agent that works on another
node submits there as its own requester rather than through a caused row. A
live caused request is therefore always on this runtime, and there is no
remote observation: a kill never routes an interrupt to a peer and never waits
for a peer to replicate a terminal back. Transitive cross-node ownership of
caused work is deferred to gents-ai/gents#3591; until it is modeled, a caused
request bound to another runtime is not a supported binding and is observed as
`unresolved` (`observeCaused`). -/
inductive KillObservation where
  | causedTerminal
  | causedLiveLocal
  | unresolved
  deriving DecidableEq, Repr

inductive KillAction where
  | settle
  | interruptCaused
  | cancelRow
  deriving DecidableEq, Repr

def KillObservation.toContract : KillObservation → String
  | .causedTerminal => "causedTerminal"
  | .causedLiveLocal => "causedLiveLocal"
  | .unresolved => "unresolved"

def KillAction.toContract : KillAction → String
  | .settle => "settle"
  | .interruptCaused => "interruptCaused"
  | .cancelRow => "cancelRow"

/-- The facts a kill reads from the caused request it resolved: whether its
lifecycle is terminal and whether it is bound to this runtime. -/
structure CausedFacts where
  terminal : Bool
  thisRuntime : Bool
  deriving DecidableEq, Repr

/-- A caused request on another runtime fails closed as `unresolved` before
its lifecycle is consulted: under the supported topology (see
`KillObservation`) such a binding cannot arise from this runtime's own
delivery, so it is neither settled from replicated state nor interrupted.
Cross-node ownership is deferred to gents-ai/gents#3591. -/
def observeCaused : Option CausedFacts → KillObservation
  | none => .unresolved
  | some facts =>
      if !facts.thisRuntime then .unresolved
      else if facts.terminal then .causedTerminal
      else .causedLiveLocal

theorem foreign_caused_request_is_unresolved (terminal : Bool) :
    observeCaused (some ⟨terminal, false⟩) = .unresolved := by
  cases terminal <;> rfl

/-- A kill acts on a caused request (settles from it or interrupts it) only
when that request is bound to this runtime. -/
theorem kill_acts_only_on_this_runtime (caused : Option CausedFacts)
    (h : observeCaused caused ≠ .unresolved) :
    ∃ facts, caused = some facts ∧ facts.thisRuntime = true := by
  match caused with
  | none => exact absurd rfl h
  | some ⟨terminal, thisRuntime⟩ =>
      cases thisRuntime
      · cases terminal <;> exact absurd rfl h
      · exact ⟨_, rfl, rfl⟩

def killAction : KillObservation → KillAction
  | .causedTerminal => .settle
  | .causedLiveLocal => .interruptCaused
  | .unresolved => .cancelRow

def KillAction.endsRowNow : KillAction → Bool
  | .interruptCaused => false
  | _ => true

theorem kill_waits_only_on_a_local_terminal (obs : KillObservation) :
    (killAction obs).endsRowNow = false ↔ obs = .causedLiveLocal := by
  cases obs <;> simp [killAction, KillAction.endsRowNow]

def killCases : List (KillObservation × KillAction) :=
  [.causedTerminal, .causedLiveLocal, .unresolved].map
    fun obs => (obs, killAction obs)

/-- Every resolved-fact shape, including a caused request bound to another
runtime, with the observation `observeCaused` derives for it. -/
def killObservationCases : List (Option CausedFacts × KillObservation) :=
  ([none] ++ [false, true].flatMap fun thisRuntime =>
      [false, true].map fun terminal => some ({ terminal, thisRuntime } : CausedFacts)).map
    fun caused => (caused, observeCaused caused)

/-! Session-message settlement records the assignment handoff in the row's
persisted `completionPending:<reason>` suffix, and the completion redrive
re-renders that suffix. A completed tool turn is not assignment evidence:
the reason names the caused session's Goal observation
(`assignment_complete` only when the canonical Goal is complete,
`assignment_absent` when the session carries none, `assignment_unknown`
when the Goal is unreadable). The persisted suffix is the authoritative
snapshot: replay re-renders it byte-identically and never re-observes live
assignment state, so the first notice and a crash-before-notice replay
agree. `ToolRecoveryCause.requestCompleted` keeps `notificationReason none`
for native background rows; only session-message settlement supplies an
assignment reason, which is why this vocabulary lives here rather than in
`ToolRecoveryCause`. -/

/-- The assignment handoff a session-message settlement may name. -/
inductive AssignmentHandoff where
  | complete
  | stopped
  | absent
  | unknown
  | live (status : String)
  deriving DecidableEq, Repr

def AssignmentHandoff.toReason : AssignmentHandoff → String
  | .complete => "assignment_complete"
  | .stopped => "assignment_stopped"
  | .absent => "assignment_absent"
  | .unknown => "assignment_unknown"
  | .live status => s!"assignment_{status}"

/-- A completed turn hands off the caused session's assignment observation;
a non-completed turn keeps its terminal reason and never reads as
assignment evidence. A complete assignment is the reasonless success form
(`assignment_complete` names the status, it is not emitted as a reason), so
only stopped, absent, unknown and live observations surface as reasons. -/
def sessionMessageHandoffReason
    (turnCompleted : Bool) (handoff : AssignmentHandoff) : Option String :=
  match turnCompleted, handoff with
  | true, .complete => none
  | true, other => some other.toReason
  | false, _ => none

theorem completed_turn_live_handoff_is_reason (status : String) :
    sessionMessageHandoffReason true (.live status) =
      some s!"assignment_{status}" := by
  rfl

theorem completed_turn_absent_handoff_is_reason :
    sessionMessageHandoffReason true .absent = some "assignment_absent" := by
  rfl

theorem completed_turn_unknown_handoff_is_reason :
    sessionMessageHandoffReason true .unknown = some "assignment_unknown" := by
  rfl

theorem completed_turn_complete_handoff_is_reasonless :
    sessionMessageHandoffReason true .complete = none := by
  rfl

theorem completed_turn_stopped_handoff_does_not_report_success :
    sessionMessageHandoffReason true .stopped = some "assignment_stopped" := by
  rfl

theorem noncompleted_turn_handoff_is_none (handoff : AssignmentHandoff) :
    sessionMessageHandoffReason false handoff = none := by
  cases handoff <;> rfl

theorem absent_reason_differs_from_unknown :
    AssignmentHandoff.absent.toReason ≠ AssignmentHandoff.unknown.toReason := by
  intro h
  simp [AssignmentHandoff.toReason] at h

/-- Live statuses come from the Goal status vocabulary, which never contains
`absent` or `unknown`, so a live reason never collides with the
distinguished forms. -/
def liveAssignmentStatuses : List String :=
  ["active", "paused", "blocked", "usage_limited", "budget_limited"]

theorem active_reason_differs_from_absent :
    (AssignmentHandoff.live "active").toReason ≠ AssignmentHandoff.absent.toReason := by
  intro h
  rw [show (AssignmentHandoff.live "active").toReason = "assignment_active" from rfl,
      show AssignmentHandoff.absent.toReason = "assignment_absent" from rfl] at h
  exact absurd h (by decide)

/-- The terminal of the request a session-message row caused. -/
inductive CausedTerminal where
  | completed
  | failed
  | dead
  | interrupted
  | superseded
  deriving DecidableEq, Repr

def CausedTerminal.recoveryCause : CausedTerminal → ToolRecoveryCause
  | .completed => .requestCompleted
  | .failed => .requestFailed
  | .dead => .requestDead
  | .interrupted => .requestInterrupted
  | .superseded => .requestSuperseded

/-- The state a settled session-message row takes, which its completion
notice names as its status. -/
def CausedTerminal.settledState (terminal : CausedTerminal) : ToolCallState :=
  terminal.recoveryCause.terminalState

/-- What settlement reads of the caused session's assignment: no Goal bound
to the caused request, a Goal it cannot read, or the bound Goal's durable
status. -/
inductive AssignmentObservation where
  | absent
  | unknown
  | active
  | paused
  | blocked
  | usageLimited
  | budgetLimited
  | complete
  | stopped
  deriving DecidableEq, Repr

def AssignmentObservation.handoff : AssignmentObservation → AssignmentHandoff
  | .absent => .absent
  | .unknown => .unknown
  | .complete => .complete
  | .stopped => .stopped
  | .active => .live "active"
  | .paused => .live "paused"
  | .blocked => .live "blocked"
  | .usageLimited => .live "usage_limited"
  | .budgetLimited => .live "budget_limited"

/-- The `<reason>` a session-message settlement persists and its notice
renders. Each constructor's wire name is the reason text. -/
inductive SettlementReason where
  | assignmentAbsent
  | assignmentUnknown
  | assignmentActive
  | assignmentPaused
  | assignmentBlocked
  | assignmentUsageLimited
  | assignmentBudgetLimited
  | assignmentStopped
  | requestFailed
  | requestDead
  | requestInterrupted
  | requestSuperseded
  deriving DecidableEq, Repr

def SettlementReason.toReason : SettlementReason → String
  | .assignmentAbsent => "assignment_absent"
  | .assignmentUnknown => "assignment_unknown"
  | .assignmentActive => "assignment_active"
  | .assignmentPaused => "assignment_paused"
  | .assignmentBlocked => "assignment_blocked"
  | .assignmentUsageLimited => "assignment_usage_limited"
  | .assignmentBudgetLimited => "assignment_budget_limited"
  | .assignmentStopped => "assignment_stopped"
  | .requestFailed => "request_failed"
  | .requestDead => "request_dead"
  | .requestInterrupted => "request_interrupted"
  | .requestSuperseded => "request_superseded"

/-- The reason a session-message settlement records. A completed turn hands
off the assignment observation (`sessionMessageHandoffReason`), so a live,
stopped, absent or unreadable assignment is never read as success and only a
complete one is reasonless; any other terminal keeps its recovery cause's
reason and the assignment is not consulted. -/
def sessionMessageSettlementReason
    (terminal : CausedTerminal) (assignment : AssignmentObservation) :
    Option SettlementReason :=
  match terminal, assignment with
  | .completed, .absent => some .assignmentAbsent
  | .completed, .unknown => some .assignmentUnknown
  | .completed, .active => some .assignmentActive
  | .completed, .paused => some .assignmentPaused
  | .completed, .blocked => some .assignmentBlocked
  | .completed, .usageLimited => some .assignmentUsageLimited
  | .completed, .budgetLimited => some .assignmentBudgetLimited
  | .completed, .stopped => some .assignmentStopped
  | .completed, .complete => none
  | .failed, _ => some .requestFailed
  | .dead, _ => some .requestDead
  | .interrupted, _ => some .requestInterrupted
  | .superseded, _ => some .requestSuperseded

theorem settlement_reason_is_handoff_or_terminal_reason
    (terminal : CausedTerminal) (assignment : AssignmentObservation) :
    (sessionMessageSettlementReason terminal assignment).map SettlementReason.toReason =
      if terminal = .completed then sessionMessageHandoffReason true assignment.handoff
      else terminal.recoveryCause.notificationReason := by
  cases terminal <;> cases assignment <;> rfl

theorem only_a_complete_assignment_is_reasonless
    (terminal : CausedTerminal) (assignment : AssignmentObservation) :
    sessionMessageSettlementReason terminal assignment = none ↔
      terminal = .completed ∧ assignment = .complete := by
  cases terminal <;> cases assignment <;> simp [sessionMessageSettlementReason]

/-- A completed turn's reason never equals a failed turn's: the assignment
and terminal vocabularies are disjoint. -/
theorem completed_turn_reason_is_not_a_terminal_reason
    (terminal : CausedTerminal) (assignment other : AssignmentObservation)
    (h : terminal ≠ .completed) :
    sessionMessageSettlementReason .completed assignment ≠
      sessionMessageSettlementReason terminal other := by
  cases terminal <;> cases assignment <;> cases other <;>
    simp_all [sessionMessageSettlementReason]

end Recovery
