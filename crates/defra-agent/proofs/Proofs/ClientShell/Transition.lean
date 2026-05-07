import Proofs.ClientShell.Submission

/-!
# Client Shell Transition Function

One-step shell state transitions and snapshot workflow advancement.
-/

/-! ## Transition function -/

/-- Session selection clears blockers and stale awaiting workflows. An
    awaiting workflow for the same session remains meaningful; an
    awaiting workflow for a different session belongs to the previous
    view and must not gate the newly selected conversation. -/
def workflowAfterSelectSession
    (sid : SessionId) : SubmissionWorkflow → SubmissionWorkflow
  | .blocked _        => .idle
  | .awaiting sid' req =>
      if sid' = sid then .awaiting sid' req else .idle
  | w                 => w

/-- The only way a snapshot may advance the workflow: an `awaiting sid
    req` retires to `.idle` when the store carries an observation for
    `sid` whose tip request is `req`. All other workflow states are
    returned unchanged — snapshots do not advance `creating`,
    `submitting`, `blocked`, or `idle`. -/
def snapshotAdvanceWorkflow
    (w : SubmissionWorkflow) (store : LocalStore) : SubmissionWorkflow :=
  match w with
  | .awaiting sid req =>
    match store.find sid with
    | some obs =>
      if obs.latestObservedRequest = some req then .idle else w
    | none     => w
  | w' => w'

/-- One-step transition. `store` and `_transport` are the current
    values at the time of the step; the shell only *reads* them to
    validate user actions, and snapshot/transport inputs never mutate
    `selection` (see C2, C3). -/
def step
    (s : ShellState) (input : ShellInput)
    (store : LocalStore) (_transport : TransportHealth)
    (ctx : SubmitContext) : ShellState :=
  match input with
  | .user .requestNewConversation =>
      match s.selection.agent with
      | some a => { s with workflow := .creating a }
      | none   => s
  | .user (.selectDeployment p a) =>
      { s with
          selection := { s.selection with peer := some p, agent := some a, session := none },
          workflow  := .idle }
  | .user (.selectSession sid) =>
      -- latch unconditionally; any blocked state clears so the user
      -- can navigate away from it; stale awaiting workflows from the
      -- previous session also clear so they cannot gate the new view
      let cleared := workflowAfterSelectSession sid s.workflow
      { s with
          selection := { s.selection with session := some sid },
          workflow  := cleared }
  | .user .startSubmit =>
      if canSubmit s store ctx then
        match s.selection.agent with
        | none   => s
        | some a => { s with workflow := .submitting a s.selection.session }
      else s
  | .user .acknowledgeBlocker =>
      match s.workflow with
      | .blocked _ => { s with workflow := .idle }
      | _          => s
  | .snapshot store' =>
      { s with workflow := snapshotAdvanceWorkflow s.workflow store' }
  | .mutation (.created sid) =>
      { s with
          selection := { s.selection with session := some sid },
          workflow  := .idle }
  | .mutation (.submitted sid req) =>
      { s with workflow := .awaiting sid req }
  | .mutation (.failed r) =>
      { s with workflow := .blocked r }
  | .transport _ => s
