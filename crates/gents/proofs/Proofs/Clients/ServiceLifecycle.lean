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

inductive StartDisposition where
  | launch | observe
  deriving DecidableEq, Repr

/-- A pending desktop start owns provisioning and readiness until it settles.
Another Start observes it; only Stop, Restart or reconfiguration may cancel it. -/
def startDisposition (starting : Bool) : StartDisposition :=
  if starting then .observe else .launch

theorem repeated_start_preserves_the_pending_owner :
    startDisposition true = .observe := by rfl

inductive Decision where
  | admitted
  /-- The caller runs inside the runtime the action would end, so the action
  would cut off the turn that asked for it. -/
  | wouldDisruptCaller
  /-- A managed update owns the service definition. -/
  | updateInProgress
  deriving DecidableEq, Repr

/-- What a lifecycle caller observes for itself about the runtime an action
would end: the service manager's running job, and whether that job's process
is an ancestor of the caller's own process. Neither the caller's environment
nor its arguments enter the observation, since an Agent's shell sets both.
Admission keeps a turn from ending its own runtime; it is not an authorization
boundary, since an unrestricted command can call the service manager directly.

External premise: the service manager reports the job's process ID, and the
operating system reports each process's parent (`proc_pidinfo` with the short
BSD record on macOS, `/proc/<pid>/stat` on Linux). The walk reads one parent at
a time while processes exit and IDs are reused; `CallerAncestry.tla` checks
that it never reports `notAncestor` for a caller still under the job, given
that neither the job nor a process between it and the caller is a child
subreaper. An orphan is then adopted above the job, so a process that left
the job's tree never re-enters it, and a parent read through a reused ID only
happens once the caller has left. -/
inductive CallerObservation where
  /-- The service manager runs no job, so no runtime an action ends can be the
  caller's. -/
  | noRuntime
  /-- The job's process is an ancestor of the caller: an Agent's shell. -/
  | ancestor
  /-- The caller's parent chain was read to its root without meeting the job. -/
  | notAncestor
  /-- The job runs but the caller's parent chain cannot be read to its end: a
  sandbox denies process information, a process of the chain is gone, the
  walk outran its bound, or the platform has no reader. -/
  | unobservable
  deriving DecidableEq, Repr

/-- An unobservable caller counts as running on the target. Admitting it could
let an Agent's turn end the runtime executing it, which
`an_agent_cannot_end_its_own_runtime` forbids; refusing an outside caller
costs a detour, since the refusal names a managed update, which survives its
caller's runtime ending, and status and the login switch stay admitted. -/
def CallerObservation.callerRunsOnTarget : CallerObservation → Bool
  | .ancestor | .unobservable => true
  | .noRuntime | .notAncestor => false

/-- `callerRunsOnTarget`: `CallerObservation.callerRunsOnTarget` of what the
caller observed. `updatePending`: a managed update of the target home has been
accepted and has not reached a terminal phase. The desktop already yielded to
a pending update on start and restart; the CLI did not. -/
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

/-- Only a caller observed outside the job may end it: a descendant of the job,
or a caller whose ancestry could not be read, is refused. -/
theorem only_an_observed_outsider_ends_the_runtime (action : Action)
    (observation : CallerObservation) (updatePending : Bool)
    (h : action.disrupts = true)
    (admitted : admission action observation.callerRunsOnTarget updatePending = .admitted) :
    observation = .noRuntime ∨ observation = .notAncestor := by
  revert h admitted
  cases action <;> cases observation <;> cases updatePending <;> decide

theorem an_unobservable_caller_cannot_end_a_runtime (action : Action) (updatePending : Bool)
    (h : action.disrupts = true) :
    admission action CallerObservation.unobservable.callerRunsOnTarget updatePending =
      .wouldDisruptCaller :=
  an_agent_cannot_end_its_own_runtime action updatePending h

theorem an_observed_outsider_can_always_stop (observation : CallerObservation) (pending : Bool)
    (h : observation.callerRunsOnTarget = false) :
    admission .stop observation.callerRunsOnTarget pending = .admitted := by
  rw [h]
  cases pending <;> decide

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
