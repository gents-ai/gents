/-! The native service lifecycle every client calls: install, start, stop,
restart, the start-at-login switch, status and uninstall of a home's runtime
service. The CLI, the desktop and the interactive start used to call the
service manager directly, each with its own checks. One handler now admits
the action for all of them. A managed update (`service update`) is a separate
operation with its own drain, backup and health gates; this model only says
when a direct action must yield to one. -/

namespace ServiceLifecycle

inductive Action where
  | install
  | start
  | stop
  | restart
  | enable
  | disable
  | status
  | uninstall
  deriving DecidableEq, Repr

/-- Actions that end the running runtime or replace what runs next. -/
def Action.disrupts : Action → Bool
  | .install | .stop | .restart | .uninstall => true
  | _ => false

/-- Actions that launch a runtime or rewrite the definition a managed update's
helper owns until it finishes. Stop and uninstall are not among them: they are
how an operator gets out of an update that cannot finish. -/
def Action.contendsWithUpdate : Action → Bool
  | .install | .start | .restart => true
  | _ => false

inductive Decision where
  | admitted
  /-- The caller runs inside the runtime the action would end, so the action
  would cut off the turn that asked for it. -/
  | wouldDisruptCaller
  /-- A managed update owns the service definition. -/
  | updateInProgress
  deriving DecidableEq, Repr

/-- `callerRunsOnTarget`: the caller is a process of the target home's
runtime (an Agent's shell). `updatePending`: a managed update of the target
home has been accepted and has not reached a terminal phase. The desktop
already yielded to a pending update on start and restart; the CLI did not. -/
def admission (action : Action) (callerRunsOnTarget updatePending : Bool) : Decision :=
  if action.disrupts && callerRunsOnTarget then .wouldDisruptCaller
  else if action.contendsWithUpdate && updatePending then .updateInProgress
  else .admitted

/-- The call a refusal names. -/
inductive Recovery where
  /-- Submit a managed update, which survives the caller's own runtime ending. -/
  | submitUpdate
  /-- Read the pending update's status, which also resumes its helper. -/
  | updateStatus
  deriving DecidableEq, Repr

def Decision.recovery : Decision → Option Recovery
  | .admitted => none
  | .wouldDisruptCaller => some .submitUpdate
  | .updateInProgress => some .updateStatus

theorem an_agent_cannot_end_its_own_runtime (action : Action) (updatePending : Bool)
    (h : action.disrupts = true) :
    admission action true updatePending = .wouldDisruptCaller := by
  revert h
  cases action <;> cases updatePending <;> decide

theorem reads_and_the_login_switch_are_always_admitted (caller pending : Bool) :
    admission .status caller pending = .admitted ∧
    admission .enable caller pending = .admitted ∧
    admission .disable caller pending = .admitted := by
  cases caller <;> cases pending <;> decide

theorem an_outside_caller_can_always_stop (pending : Bool) :
    admission .stop false pending = .admitted ∧
    admission .uninstall false pending = .admitted := by
  cases pending <;> decide

theorem nothing_starts_under_a_pending_update (action : Action)
    (h : action.contendsWithUpdate = true) (caller : Bool) :
    admission action caller true ≠ .admitted := by
  revert h
  cases action <;> cases caller <;> decide

theorem every_refusal_names_a_call (action : Action) (caller pending : Bool) :
    admission action caller pending = .admitted ∨
    (admission action caller pending).recovery.isSome = true := by
  cases action <;> cases caller <;> cases pending <;> decide

end ServiceLifecycle
