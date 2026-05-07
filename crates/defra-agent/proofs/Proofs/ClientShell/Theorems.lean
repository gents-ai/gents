import Proofs.ClientShell.Projection

/-!
# Client Shell Theorems

Shell workflow safety and projection properties.
-/

/-! ## Theorems

Numbering uses a `C` prefix to parallel `Client.lean`'s T-series.
The load-bearing ones are C2, C3, C4, C5, C6, C9 — these collectively
rule out the render-time repair bug class. -/

/-- **C1 Projection purity.** `projectChat` is a pure function — equal
    inputs produce equal outputs. Trivial by construction; stated to
    document the contract the Rust side must preserve (no
    `ProjectionApplied`-style writeback from rendering). -/
theorem projection_pure
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) :
    projectChat s store ctx = projectChat s store ctx := rfl

/-- **C2 Snapshot preserves selection.** A snapshot input may advance
    the workflow but never rewrites `selection`. This is *the* fix
    for the "click between conversations while P2P degraded" bug:
    snapshot arrivals cannot invalidate a freshly-made user
    selection. -/
theorem snapshot_preserves_selection
    (s : ShellState) (store store' : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) :
    (step s (.snapshot store') store h ctx).selection = s.selection := rfl

/-- **C2′ Snapshot workflow envelope.** The only workflow change a
    snapshot can make is `snapshotAdvanceWorkflow`. Combined with the
    shape of that function, this means snapshots can only take
    `awaiting sid req` to `idle`, and only on matching tip (see C9). -/
theorem snapshot_workflow_envelope
    (s : ShellState) (store store' : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) :
    (step s (.snapshot store') store h ctx).workflow =
      snapshotAdvanceWorkflow s.workflow store' := rfl

/-- **C3 Transport non-mutation.** A transport input never mutates
    `ShellState`. Transport health is not in the state machine
    alphabet. -/
theorem transport_is_noop
    (s : ShellState) (store : LocalStore) (h h' : TransportHealth)
    (ctx : SubmitContext) :
    step s (.transport h') store h ctx = s := rfl

/-- **C4 Local switching independence.** Selecting a session succeeds
    regardless of transport health. The result does not depend on
    the `TransportHealth` parameter at all. -/
theorem local_switch_independent_of_transport
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (h h' : TransportHealth) :
    step s (.user (.selectSession sid)) store h  ctx
      = step s (.user (.selectSession sid)) store h' ctx := rfl

/-- **C4′ Selection latches unconditionally.** A `selectSession`
    action latches the chosen session regardless of store contents
    or transport. What is in the store only affects `SelectionHealth`
    in projection — the selection itself is the user's word. -/
theorem select_session_latches
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (h : TransportHealth) :
    (step s (.user (.selectSession sid)) store h ctx).selection.session
      = some sid := rfl

/-- Selecting a deployment clears the selected session. Deployment
    switches are an explicit shell transition boundary: the next
    conversation must be chosen against the newly selected agent. -/
theorem select_deployment_clears_session
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (peer : PeerId) (agent : AgentDid) (h : TransportHealth) :
    (step s (.user (.selectDeployment peer agent)) store h ctx).selection.session
      = none := rfl

/-- **C5 Selection stickiness under in-flight submission.** While the
    workflow is creating / submitting / awaiting, snapshot arrivals
    do not rewrite the selection. This is a corollary of C2; stated
    separately because it is the load-bearing property for the
    "pending session must remain selected" user requirement. -/
theorem selection_sticky_under_inflight
    (s : ShellState) (store store' : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext)
    (_h_inflight :
      (∃ a, s.workflow = .creating a)
      ∨ (∃ a opt, s.workflow = .submitting a opt)
      ∨ (∃ sid req, s.workflow = .awaiting sid req)) :
    (step s (.snapshot store') store h ctx).selection = s.selection :=
  snapshot_preserves_selection s store store' h ctx

/-- **C6 Submission safety.** `startSubmit` only mutates the workflow
    when `canSubmit` is true. When the gate is closed, the state is
    returned unchanged. -/
theorem start_submit_gated
    (s : ShellState) (store : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) :
    canSubmit s store ctx = false →
    step s (.user .startSubmit) store h ctx = s := by
  intro h_cannot
  show (if canSubmit s store ctx then _ else s) = s
  rw [h_cannot]
  rfl

/-- **C7 Follow-up safety is transport-independent by signature.**
    `trustworthyForFollowUp` has no `TransportHealth` argument. The
    proof is trivial; the theorem's purpose is to document that
    signature, so future drift (adding a `TransportHealth` argument)
    would break this statement. -/
theorem trustworthy_transport_irrelevant
    (s : ShellState) (store : LocalStore) (b : Option BehaviorId) :
    trustworthyForFollowUp s store b = trustworthyForFollowUp s store b :=
  rfl

/-- **C8 Observed selection is resolved.** If the shell's selected
    session is in the store, projection reports `resolved`. -/
theorem selected_in_store_is_resolved
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (obs : SessionObservation)
    (h_sel  : s.selection.session = some sid)
    (h_find : store.find sid = some obs) :
    (projectChat s store ctx).selectionHealth = .resolved := by
  simp [projectChat, classifySelection, h_sel, h_find]

/-- **C9 Workflow advances only on matching observation.** An
    `awaiting sid req` workflow can only retire to `.idle` on a
    snapshot whose store contains an observation for `sid` with tip
    `req`. This is what makes `latestObservedRequest` load-bearing
    in `SessionObservation`. -/
theorem awaiting_retires_only_on_matching_tip
    (sid : SessionId) (req : RequestId)
    (s : ShellState) (store store' : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext)
    (h_wf   : s.workflow = .awaiting sid req)
    (h_idle : (step s (.snapshot store') store h ctx).workflow = .idle) :
    ∃ obs, store'.find sid = some obs
         ∧ obs.latestObservedRequest = some req := by
  have h_adv : snapshotAdvanceWorkflow s.workflow store' = .idle := h_idle
  rw [h_wf] at h_adv
  cases h_find : store'.find sid with
  | none =>
    simp [snapshotAdvanceWorkflow, h_find] at h_adv
  | some obs =>
    by_cases h_tip : obs.latestObservedRequest = some req
    · exact ⟨obs, rfl, h_tip⟩
    · simp [snapshotAdvanceWorkflow, h_find, h_tip] at h_adv

/-- **C10 Projection reflects observed tip.** When the selected
    session has an observation in the store, the view's reported
    `turnState` equals that observation's `latestTurn`. This is a
    simple lift of `Client.lean`'s per-turn projection into the
    multi-session shell. -/
theorem projection_reflects_observed_tip
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (obs : SessionObservation)
    (h_sel  : s.selection.session = some sid)
    (h_find : store.find sid = some obs) :
    (projectChat s store ctx).turnState = obs.latestTurn := by
  simp [projectChat, classifySelection, h_sel, h_find]

/-! ## Supporting observations

Small facts that make the Rust refactor contract explicit. -/

/-- Mutation `.submitted` sets the workflow to `.awaiting` and does
    not touch the selection. The controller callback path in Rust
    should therefore never reach into `selection` when handling a
    submission-success result. -/
theorem mutation_submitted_preserves_selection
    (s : ShellState) (store : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) (sid : SessionId) (req : RequestId) :
    (step s (.mutation (.submitted sid req)) store h ctx).selection
      = s.selection := rfl

/-- Mutation `.failed` sets the workflow to `.blocked` and does not
    touch the selection. -/
theorem mutation_failed_preserves_selection
    (s : ShellState) (store : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) (r : BlockedReason) :
    (step s (.mutation (.failed r)) store h ctx).selection
      = s.selection := rfl

/-- A `selectSession` that lands while the workflow is `.blocked`
    clears the blocker. This is the one place a user action removes
    a blocker without an explicit `acknowledgeBlocker`; the
    justification is that picking a different conversation is itself
    an acknowledgment that the previous blocker no longer applies. -/
theorem select_session_clears_blocker
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (r : BlockedReason) (h : TransportHealth)
    (h_wf : s.workflow = .blocked r) :
    (step s (.user (.selectSession sid)) store h ctx).workflow = .idle := by
  simp [step, workflowAfterSelectSession, h_wf]

/-- Selecting a different session clears an awaiting workflow tied to
    the previous session. This is the Lean-side contract for the
    frontend projection's stale-workflow-after-switch behavior. -/
theorem select_session_clears_stale_awaiting
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (oldSid newSid : SessionId) (req : RequestId) (h : TransportHealth)
    (h_wf : s.workflow = .awaiting oldSid req)
    (h_ne : oldSid ≠ newSid) :
    (step s (.user (.selectSession newSid)) store h ctx).workflow = .idle := by
  simp [step, workflowAfterSelectSession, h_wf, h_ne]

/-- Re-selecting the same session preserves its awaiting workflow. The
    stale-workflow cleanup only applies when the selected session changes. -/
theorem select_session_preserves_same_session_awaiting
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (req : RequestId) (h : TransportHealth)
    (h_wf : s.workflow = .awaiting sid req) :
    (step s (.user (.selectSession sid)) store h ctx).workflow = .awaiting sid req := by
  simp [step, workflowAfterSelectSession, h_wf]
