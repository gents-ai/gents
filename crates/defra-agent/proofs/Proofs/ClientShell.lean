import Proofs.Basic
import Proofs.Client

/-!
# Layer 4: Client-Shell Workflow

A formal model of the multi-session client shell that sits *above*
`Proofs.Client`'s per-turn observation model.

## Relationship to other layers

The proof suite already separates three layers relevant here:

1. **Server truth** — the request/process/persistence lifecycles
   proven in `Proofs.Request`, `Proofs.Process`, `Proofs.Persistence`.
2. **Replicated local-store truth** — the per-turn projection proven
   in `Proofs.Client`. A pure function of one turn's observed
   documents; transport stalls are not turn-state.
3. **Client-shell workflow truth (this file)** — the multi-session
   shell that holds the user's selection, tracks in-flight submission
   intent, and decides whether a follow-up is safe.

The shell consumes layer 2 as a value (`SessionObservation.latestTurn`)
— it does not re-derive turn state, nor does it weaken the per-turn
theorems.

## What this layer promises

Load-bearing guarantees (theorems below):

- a snapshot input may advance the submission workflow but may never
  mutate the selection (C2, C2′, C5)
- transport health is a non-mutating input to the state machine (C3)
- client availability is a separate input from transport health (by
  construction: `SubmitContext.clientAvailable` vs. `TransportHealth`)
- projection (rendering) is a pure function — never a source of state
  change (C1)
- local chat switching between two observed sessions succeeds
  regardless of transport health (C4, C4′)
- a follow-up submission gate (`canSubmit`) is transport-independent
  by signature (C7)
- `awaiting` only retires on a matching tip observation (C9)

## What this layer deliberately does not model

- composer text, behavior override, expanded cards, scroll position,
  stick-to-bottom — editor state, not state-machine state
- P2P auto-restart debounce / cooldown, client-core watchdog retries
  — escalation policy, handled by Rust tests
- first-launch onboarding redirect — one-shot UX
- live turn derivation — already in `Proofs.Client`
- transport diagnostics beyond a decoupled `TransportIndicator`
  projection that is out of the core theorem surface
-/

/-- Peer identifier. Opaque — only equality matters. -/
abbrev PeerId := Nat

/-- Agent DID identifier. Opaque — only equality matters. -/
abbrev AgentDid := Nat

/-- What the shell sees about one session from the replicated store.

    `latestTurn` is the value of `Proofs.Client.deriveTurn` for this
    session's tip attempt, imported as data — the shell does not
    re-derive it.

    `latestObservedRequest` is the tip `RequestId` carried alongside
    `latestTurn`. It is what allows the shell to prove workflow
    advancement (C9): we can state "the awaited request was
    observed" without replaying turn derivation. -/
structure SessionObservation where
  sessionId             : SessionId
  agentDid              : AgentDid
  behaviorId            : Option BehaviorId
  latestObservedRequest : Option RequestId
  latestTurn            : Option ClientTurnState
  deriving DecidableEq, Repr

/-- Replicated local-store truth consumed by the shell. Sessions are a
    list; `find` returns the first match, so duplicate `SessionId`
    entries are resolved deterministically. -/
structure LocalStore where
  deployments : List (PeerId × AgentDid)
  sessions    : List SessionObservation
  deriving Repr

namespace LocalStore

/-- Lookup a session in the store by id. -/
def find (store : LocalStore) (sid : SessionId) : Option SessionObservation :=
  store.sessions.find? (fun obs => obs.sessionId == sid)

/-- Decidable membership of a session id in the store. -/
def hasSession (store : LocalStore) (sid : SessionId) : Bool :=
  (store.find sid).isSome

end LocalStore

/-- Transport health. Deliberately coarse — soft vs. hard is the
    minimum needed for escalation policy. Finer gradations (last
    error string, consecutive failure count) are Rust diagnostics. -/
inductive TransportHealth where
  | healthy
  | degraded
  | wedged
  deriving DecidableEq, Repr

/-- The user's selection — the shell's anchor. Never mutated by
    snapshot or transport inputs (see C2, C3). -/
structure Selection where
  peer    : Option PeerId
  agent   : Option AgentDid
  session : Option SessionId
  deriving DecidableEq, Repr

/-- Reasons a submission workflow can be blocked and require the user
    to acknowledge before continuing. -/
inductive BlockedReason where
  | clientOffline
  | behaviorMismatch (requested existing : BehaviorId)
  | mutationRejected
  deriving DecidableEq, Repr

/-- Local submission workflow. Five cases.

    `TurnInProgress` is deliberately **not** present here. It is a
    projection of the replicated store, not a shell state. Rendering
    a "streaming" bubble belongs in `ChatView`, not `ShellState`. -/
inductive SubmissionWorkflow where
  | idle
  | creating   (agent : AgentDid)
  | submitting (agent : AgentDid) (session : Option SessionId)
  | awaiting   (session : SessionId) (request : RequestId)
  | blocked    (reason  : BlockedReason)
  deriving DecidableEq, Repr

/-- The shell's state-machine surface. Minimal on purpose. -/
structure ShellState where
  selection : Selection
  workflow  : SubmissionWorkflow
  deriving DecidableEq, Repr

namespace ShellState

/-- The initial shell state: no selection, idle workflow. -/
def initial : ShellState :=
  { selection := { peer := none, agent := none, session := none },
    workflow  := .idle }

end ShellState

/-! ## Actions and inputs -/

inductive UserAction where
  | selectDeployment (peer : PeerId) (agent : AgentDid)
  | selectSession    (session : SessionId)
  | requestNewConversation
  | startSubmit
  | acknowledgeBlocker
  deriving DecidableEq, Repr

/-- Result of a mutation the shell initiated. Mutation *progress*
    (spinner, disabled button) is derived from `workflow` by
    projection; it is not a separate input. -/
inductive MutationResult where
  | created   (session : SessionId)
  | submitted (session : SessionId) (request : RequestId)
  | failed    (reason  : BlockedReason)
  deriving DecidableEq, Repr

/-- Everything that can feed into `step`. Transport is here for
    completeness; it is structurally non-mutating (see C3). -/
inductive ShellInput where
  | user      (action : UserAction)
  | snapshot  (store  : LocalStore)
  | mutation  (result : MutationResult)
  | transport (health : TransportHealth)
  deriving Repr

/-! ## Submission gate

Client availability is a separate input from transport health. A
"healthy transport, offline client" is a legal configuration (e.g.
during a voluntary restart) and must still block submission. -/

structure SubmitContext where
  clientAvailable   : Bool
  composerNonEmpty  : Bool
  requestedBehavior : Option BehaviorId
  deriving Repr

/-- Behavior-mismatch check: if the composer has an override that
    differs from the session's persisted behavior, block. If either
    side is absent, no mismatch is reported. -/
def behaviorMismatch
    (store : LocalStore) (sid : SessionId)
    (requested : Option BehaviorId) : Bool :=
  match requested, (store.find sid).bind (·.behaviorId) with
  | some r, some e => decide (r ≠ e)
  | _, _           => false

/-- Whether the selected session is trustworthy enough for follow-up
    submission. Transport-independent by signature. -/
def trustworthyForFollowUp
    (s : ShellState) (store : LocalStore)
    (requestedBehavior : Option BehaviorId) : Bool :=
  match s.selection.session with
  | none     => true
  | some sid =>
    match store.find sid with
    | none     => false
    | some obs =>
      let tipCoherent :=
        match obs.latestObservedRequest, obs.latestTurn with
        | some _, some _ => true
        | none,   none   => true
        | _,      _      => false
      let noMismatch := ¬ behaviorMismatch store sid requestedBehavior
      tipCoherent && noMismatch

/-- Submission safety predicate. Transport-independent by signature —
    only `SubmitContext.clientAvailable` represents "can we even emit
    a mutation right now." -/
def canSubmit
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : Bool :=
  if ¬ ctx.clientAvailable then false
  else if s.selection.agent.isNone then false
  else if ¬ ctx.composerNonEmpty then false
  else
    match s.workflow with
    | .creating _ | .submitting _ _ | .awaiting _ _ | .blocked _ => false
    | .idle =>
      match s.selection.session with
      | none     => true  -- new conversation: no session to validate
      | some sid =>
        match store.find sid with
        | none     => false
        | some obs =>
          let tipTerminalOrUnstarted :=
            match obs.latestObservedRequest, obs.latestTurn with
            | some _, some t => t.isTerminal
            | none,   none   => true
            | _,      _      => false
          let noMismatch := ¬ behaviorMismatch store sid ctx.requestedBehavior
          tipTerminalOrUnstarted && noMismatch

/-! ## Transition function -/

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
      -- can navigate away from it
      let cleared : SubmissionWorkflow :=
        match s.workflow with
        | .blocked _ => .idle
        | w          => w
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

/-! ## Projection -/

inductive SelectionHealth where
  | noSelection
  | resolved
  | pendingObservation
  | absent
  deriving DecidableEq, Repr

inductive SendBlockedReason where
  | clientOffline
  | agentNotSelected
  | composerEmpty
  | mutationInFlight
  | awaitingObservation
  | awaitingTurnTerminality (turn : ClientTurnState)
  | sessionBehaviorMismatch
  | sessionAbsent
  | inconsistentObservation
  | workflowBlocked
  deriving DecidableEq, Repr

inductive SendDecision where
  | ready
  | blocked (reason : SendBlockedReason)
  deriving DecidableEq, Repr

/-- The visible chat view. Pure function of
    `(shell, store, submit-context)`. Does **not** take
    `TransportHealth` — the load-bearing components are
    transport-independent. Transport diagnostics are a separate,
    decoupled projection (`projectTransportIndicator`). -/
structure ChatView where
  selection       : Selection
  selectionHealth : SelectionHealth
  visibleSession  : Option SessionObservation
  turnState       : Option ClientTurnState
  workflow        : SubmissionWorkflow
  sendDecision    : SendDecision
  deriving DecidableEq, Repr

/-- Whether the shell's workflow currently references `sid`, so a
    missing-from-store selection can be distinguished between
    "pending observation" (we're expecting it) and "absent". -/
def workflowReferences (w : SubmissionWorkflow) (sid : SessionId) : Bool :=
  match w with
  | .submitting _ (some sid') => decide (sid = sid')
  | .awaiting sid' _          => decide (sid = sid')
  | _                         => false

/-- Classify the selected session against the current store and
    workflow. Returns both the health and the visible observation
    (when resolved). -/
def classifySelection
    (sel : Selection) (store : LocalStore) (w : SubmissionWorkflow)
    : SelectionHealth × Option SessionObservation :=
  match sel.session with
  | none     => (.noSelection, none)
  | some sid =>
    match store.find sid with
    | some obs => (.resolved, some obs)
    | none     =>
      if workflowReferences w sid then (.pendingObservation, none)
      else (.absent, none)

/-- Derive the send decision. Transport does not appear —
    `clientAvailable` is the only "can we even submit" signal. -/
def projectSendDecision
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : SendDecision :=
  if ¬ ctx.clientAvailable then .blocked .clientOffline
  else if s.selection.agent.isNone then .blocked .agentNotSelected
  else if ¬ ctx.composerNonEmpty then .blocked .composerEmpty
  else match s.workflow with
    | .creating _ | .submitting _ _ => .blocked .mutationInFlight
    | .awaiting _ _                 => .blocked .awaitingObservation
    | .blocked _                    => .blocked .workflowBlocked
    | .idle =>
      match s.selection.session with
      | none     => .ready
      | some sid =>
        match store.find sid with
        | none     =>
          if workflowReferences s.workflow sid then
            .blocked .awaitingObservation
          else .blocked .sessionAbsent
        | some obs =>
          if behaviorMismatch store sid ctx.requestedBehavior then
            .blocked .sessionBehaviorMismatch
          else
            match obs.latestObservedRequest, obs.latestTurn with
            | none,   none   => .ready
            | some _, some t =>
              if t.isTerminal then .ready
              else .blocked (.awaitingTurnTerminality t)
            | _,      _      => .blocked .inconsistentObservation

/-- The core projection. Pure function of
    `(shell, store, submit-context)`. No `TransportHealth` argument. -/
def projectChat
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : ChatView :=
  let classified := classifySelection s.selection store s.workflow
  { selection       := s.selection,
    selectionHealth := classified.fst,
    visibleSession  := classified.snd,
    turnState       := classified.snd.bind (·.latestTurn),
    workflow        := s.workflow,
    sendDecision    := projectSendDecision s store ctx }

/-! ## Transport diagnostics (decoupled)

`TransportIndicator` is a separate projection that takes only
`TransportHealth` and never appears in `ChatView`. This keeps the
core theorem surface free of transport concerns. -/

inductive TransportIndicator where
  | quiet
  | degradedNotice
  | wedgedNotice
  deriving DecidableEq, Repr

def projectTransportIndicator : TransportHealth → TransportIndicator
  | .healthy  => .quiet
  | .degraded => .degradedNotice
  | .wedged   => .wedgedNotice

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
  show
    (match s.workflow with | .blocked _ => (.idle : SubmissionWorkflow) | w => w)
      = .idle
  rw [h_wf]
