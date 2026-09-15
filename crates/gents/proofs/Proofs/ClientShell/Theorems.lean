import Proofs.ClientShell.Projection

/-- Once the matching request is observed terminal, a snapshot retires the local
awaiting latch and permits a follow-up under the ordinary admission premises.
This is a projection guarantee, not a transport delivery deadline. -/
theorem matching_terminal_snapshot_allows_follow_up
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (req : RequestId) (obs : SessionObservation)
    (turn : ClientTurnState)
    (hsel : s.selection.session = some sid)
    (hagent : s.selection.agent.isNone = false)
    (hw : s.workflow = .awaiting sid req)
    (hfind : store.find sid = some obs)
    (hreq : obs.latestObservedRequest = some req)
    (hturn : obs.latestTurn = some turn)
    (hterminal : turn.isTerminal = true)
    (hclient : ctx.clientAvailable = true)
    (htext : ctx.composerNonEmpty = true)
    (hbehavior : behaviorMismatch store sid ctx.requestedBehavior = false) :
    projectSendDecision (step s (.snapshot store) store .healthy ctx) store ctx = .ready := by
  simp [step, snapshotAdvanceWorkflow, hw, hfind, hreq, projectSendDecision,
    hsel, hagent, hclient, htext, hbehavior, hturn, hterminal]

/-- A terminal observation for another request cannot acknowledge this submit. -/
theorem unrelated_terminal_does_not_retire_awaiting
    (store : LocalStore) (sid : SessionId) (req : RequestId)
    (obs : SessionObservation) (hfind : store.find sid = some obs)
    (hother : obs.latestObservedRequest ≠ some req) :
    snapshotAdvanceWorkflow (.awaiting sid req) store = .awaiting sid req := by
  simp [snapshotAdvanceWorkflow, hfind, hother]

/-- A locally observed request can precede its mutation acknowledgment. The
acknowledgment consumes the current observation instead of reinstating a latch
that would need another notification to clear. -/
theorem observed_before_ack_retires_without_another_snapshot
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (req : RequestId) (obs : SessionObservation)
    (hfind : store.find sid = some obs)
    (hreq : obs.latestObservedRequest = some req) :
    (step s (.mutation (.submitted sid req)) store .healthy ctx).workflow = .idle := by
  simp [step, snapshotAdvanceWorkflow, hfind, hreq]

theorem acknowledgment_and_matching_snapshot_commute
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (req : RequestId) :
    (step s (.mutation (.submitted sid req)) store .healthy ctx).workflow =
      (step { s with workflow := .awaiting sid req }
        (.snapshot store) store .healthy ctx).workflow := rfl

theorem snapshot_preserves_selection
    (s : ShellState) (store store' : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) :
    (step s (.snapshot store') store h ctx).selection = s.selection := rfl

theorem snapshot_workflow_envelope
    (s : ShellState) (store store' : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) :
    (step s (.snapshot store') store h ctx).workflow =
      snapshotAdvanceWorkflow s.workflow store' := rfl

theorem transport_is_noop
    (s : ShellState) (store : LocalStore) (h h' : TransportHealth)
    (ctx : SubmitContext) :
    step s (.transport h') store h ctx = s := rfl

theorem local_switch_independent_of_transport
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (h h' : TransportHealth) :
    step s (.user (.selectSession sid)) store h  ctx
      = step s (.user (.selectSession sid)) store h' ctx := rfl

theorem select_session_latches
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (h : TransportHealth) :
    (step s (.user (.selectSession sid)) store h ctx).selection.session
      = some sid := rfl

theorem select_principal_route_clears_session
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (peer : PeerId) (agent : AgentDid) (h : TransportHealth) :
    (step s (.user (.selectPrincipalRoute peer agent)) store h ctx).selection.session
      = none := rfl

theorem selection_sticky_under_inflight
    (s : ShellState) (store store' : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext)
    (_h_inflight :
      (∃ a opt, s.workflow = .submitting a opt)
      ∨ (∃ sid req, s.workflow = .awaiting sid req)) :
    (step s (.snapshot store') store h ctx).selection = s.selection :=
  snapshot_preserves_selection s store store' h ctx

theorem start_submit_gated
    (s : ShellState) (store : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) :
    canSubmit s store ctx = false →
    step s (.user .startSubmit) store h ctx = s := by
  intro h_cannot
  show (if canSubmit s store ctx then _ else s) = s
  rw [h_cannot]
  rfl

theorem selected_in_store_is_resolved
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (obs : SessionObservation)
    (h_sel  : s.selection.session = some sid)
    (h_find : store.find sid = some obs) :
    (projectChat s store ctx).selectionHealth = .resolved := by
  simp [projectChat, classifySelection, h_sel, h_find]

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

theorem projection_reflects_observed_tip
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (obs : SessionObservation)
    (h_sel  : s.selection.session = some sid)
    (h_find : store.find sid = some obs) :
    (projectChat s store ctx).turnState = obs.latestTurn := by
  simp [projectChat, classifySelection, h_sel, h_find]

theorem mutation_submitted_selects_session
    (s : ShellState) (store : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) (sid : SessionId) (req : RequestId) :
    (step s (.mutation (.submitted sid req)) store h ctx).selection.session
      = some sid := rfl

theorem new_session_composer_clears_selection
    (s : ShellState) (store : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) :
    (step s (.user .requestNewSession) store h ctx).selection.session
      = none := rfl

theorem mutation_failed_preserves_selection
    (s : ShellState) (store : LocalStore) (h : TransportHealth)
    (ctx : SubmitContext) (r : BlockedReason) :
    (step s (.mutation (.failed r)) store h ctx).selection
      = s.selection := rfl

theorem select_session_clears_blocker
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (r : BlockedReason) (h : TransportHealth)
    (h_wf : s.workflow = .blocked r) :
    (step s (.user (.selectSession sid)) store h ctx).workflow = .idle := by
  simp [step, workflowAfterSelectSession, h_wf]

theorem select_session_clears_stale_awaiting
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (oldSid newSid : SessionId) (req : RequestId) (h : TransportHealth)
    (h_wf : s.workflow = .awaiting oldSid req)
    (h_ne : oldSid ≠ newSid) :
    (step s (.user (.selectSession newSid)) store h ctx).workflow = .idle := by
  simp [step, workflowAfterSelectSession, h_wf, h_ne]

theorem select_session_preserves_same_session_awaiting
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (req : RequestId) (h : TransportHealth)
    (h_wf : s.workflow = .awaiting sid req) :
    (step s (.user (.selectSession sid)) store h ctx).workflow = .awaiting sid req := by
  simp [step, workflowAfterSelectSession, h_wf]
