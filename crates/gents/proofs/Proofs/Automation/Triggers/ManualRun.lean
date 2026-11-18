/-! Firing one Task now, on a person's or an Agent's request. Every client
reaches the one handler: it decides whether the Task may run, which Task a
Schedule names, and how the session the fire opens is titled. -/

namespace Triggers.ManualRun

/-- A Task or Agent as read under the selected node. A document another node
owns under the same name is `missing`. -/
inductive Presence where
  | missing
  | disabled
  | enabled
  deriving DecidableEq, Repr

inductive RunDecision where
  | run
  | taskMissing
  | taskDisabled
  | agentMissing
  | agentDisabled
  /-- A command Task runs on the Node from its Trigger; a client's manual run
  writes a request for an Agent to answer, and a command Task has none. -/
  | commandTask
  deriving DecidableEq, Repr

/-- A Task runs only when it and the Agent that answers it are enabled. The
Task is judged first, so a refusal names the document the caller chose; an
enabled command Task is refused before any Agent is looked for. -/
def runDecision (task agent : Presence) (command : Bool) : RunDecision :=
  match task, command, agent with
  | .missing, _, _ => .taskMissing
  | .disabled, _, _ => .taskDisabled
  | .enabled, true, _ => .commandTask
  | .enabled, false, .missing => .agentMissing
  | .enabled, false, .disabled => .agentDisabled
  | .enabled, false, .enabled => .run

theorem run_requires_both_enabled (task agent : Presence) (command : Bool)
    (h : runDecision task agent command = .run) :
    task = .enabled ∧ agent = .enabled ∧ command = false := by
  cases task <;> cases agent <;> cases command <;> simp_all [runDecision]

inductive Matches where
  | none
  | one
  | many
  deriving DecidableEq, Repr

inductive ScheduleDecision where
  | run
  | scheduleMissing
  | scheduleAmbiguous
  | noEnabledTrigger
  /-- Several enabled Triggers share the Schedule; the caller names a Task. -/
  | manyEnabledTriggers
  deriving DecidableEq, Repr

/-- Running a Schedule now runs the Task of its one enabled Trigger. -/
def scheduleDecision (schedules enabledTriggers : Matches) : ScheduleDecision :=
  match schedules, enabledTriggers with
  | .none, _ => .scheduleMissing
  | .many, _ => .scheduleAmbiguous
  | .one, .none => .noEnabledTrigger
  | .one, .many => .manyEnabledTriggers
  | .one, .one => .run

theorem schedule_run_is_unambiguous (schedules enabledTriggers : Matches)
    (h : scheduleDecision schedules enabledTriggers = .run) :
    schedules = .one ∧ enabledTriggers = .one := by
  cases schedules <;> cases enabledTriggers <;> simp_all [scheduleDecision]

inductive TitleKind where
  /-- The fire joins a session that already has its title. -/
  | unchanged
  /-- The Task's label and the time of the fire, as a Trigger's fire titles it. -/
  | run
  /-- The Task's label and the invocation key, stable across retries. -/
  | goal
  deriving DecidableEq, Repr

/-- A manual fire titles the session it opens exactly as a Trigger's fire of
the same Task does, whichever client asked. -/
def sessionTitle (continuesSession goalBacked : Bool) : TitleKind :=
  if continuesSession then .unchanged
  else if goalBacked then .goal
  else .run

theorem continued_session_keeps_its_title (goalBacked : Bool) :
    sessionTitle true goalBacked = .unchanged := rfl

end Triggers.ManualRun
