import Proofs.Recovery.Sweeps.ToolCalls

namespace Recovery

open ToolExecution

/-! A session-message row settles on its caused request's terminal, or fails
closed when it cannot name that request (`ToolRecoveryCause.causedRequestUnbound`).
Accepted premise: a peer that never replicates its caused request's terminal
back leaves the row running; only a kill ends it (`killAction`). -/

def sessionMessageRecoverySweep : RecoverySweep :=
  toolCallRecoverySweepFor true
    "tool_call_lifecycle_recover_session_message_rows"
    "background_completion::settle_running_session_message_rows" .periodic

inductive KillObservation where
  | causedTerminal
  | causedLiveLocal
  | causedLiveRemote
  | unresolved
  deriving DecidableEq, Repr

inductive KillAction where
  | settle
  | interruptCaused
  | interruptAndCancelRow
  | cancelRow
  deriving DecidableEq, Repr

def KillObservation.toContract : KillObservation → String
  | .causedTerminal => "causedTerminal"
  | .causedLiveLocal => "causedLiveLocal"
  | .causedLiveRemote => "causedLiveRemote"
  | .unresolved => "unresolved"

def KillAction.toContract : KillAction → String
  | .settle => "settle"
  | .interruptCaused => "interruptCaused"
  | .interruptAndCancelRow => "interruptAndCancelRow"
  | .cancelRow => "cancelRow"

def killAction : KillObservation → KillAction
  | .causedTerminal => .settle
  | .causedLiveLocal => .interruptCaused
  | .causedLiveRemote => .interruptAndCancelRow
  | .unresolved => .cancelRow

def KillAction.endsRowNow : KillAction → Bool
  | .interruptCaused => false
  | _ => true

theorem kill_waits_only_on_a_local_terminal (obs : KillObservation) :
    (killAction obs).endsRowNow = false ↔ obs = .causedLiveLocal := by
  cases obs <;> simp [killAction, KillAction.endsRowNow]

def killCases : List (KillObservation × KillAction) :=
  [.causedTerminal, .causedLiveLocal, .causedLiveRemote, .unresolved].map
    fun obs => (obs, killAction obs)

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
  | absent
  | unknown
  | live (status : String)
  deriving DecidableEq, Repr

def AssignmentHandoff.toReason : AssignmentHandoff → String
  | .complete => "assignment_complete"
  | .absent => "assignment_absent"
  | .unknown => "assignment_unknown"
  | .live status => s!"assignment_{status}"

/-- A completed turn hands off the caused session's assignment observation;
a non-completed turn keeps its terminal reason and never reads as
assignment evidence. A complete assignment is the reasonless success form
(`assignment_complete` names the status, it is not emitted as a reason), so
only absent, unknown and live observations surface as reasons. -/
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

end Recovery
