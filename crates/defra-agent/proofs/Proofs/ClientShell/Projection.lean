import Proofs.ClientShell.Transition

/-!
# Client Shell Projection

Pure chat-view and transport diagnostic projections.
-/

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

/-! ## Active Overlay

`projectActiveOverlay` is a pure function over the tip response and the
derived turn state. It returns `some` when the live overlay block should be
rendered, otherwise `none`. The body of the block is intentionally elided —
the formal model only needs the *presence* decision, not the byte content. -/

structure OverlayBlock where
  hasContent   : Bool
  hasReasoning : Bool
  deriving DecidableEq, Repr

/-- Decide whether the live overlay should be rendered, and if so what
    payload presence flags it carries. -/
def projectActiveOverlay
    (resp : Option ResponseSnapshot)
    (turn : Option ClientTurnState)
    (materialized : Bool)
    (hasContent hasReasoning : Bool)
    : Option OverlayBlock :=
  match resp with
  | none => none
  | some r =>
    if materialized then none
    else if r.status = .complete ∨ r.status = .error then none
    else
      match turn with
      | none => none
      | some t =>
        if t.isTerminal then none
        else if t = .waitingForClaim ∨ t = .streaming then
          if hasContent ∨ hasReasoning then
            some { hasContent := hasContent, hasReasoning := hasReasoning }
          else none
        else none

/-! ## Theorems O1–O3 -/

/-- O1: `projectActiveOverlay` returns at most one `OverlayBlock`. Trivial by
    construction (the function returns at most one `some`). Stated for the
    contract surface. -/
theorem projectActiveOverlay_at_most_one
    (resp : Option ResponseSnapshot)
    (turn : Option ClientTurnState)
    (materialized hasContent hasReasoning : Bool) :
    ∀ b₁ b₂,
      projectActiveOverlay resp turn materialized hasContent hasReasoning = some b₁ →
      projectActiveOverlay resp turn materialized hasContent hasReasoning = some b₂ →
      b₁ = b₂ := by
  intros b₁ b₂ h₁ h₂
  rw [h₁] at h₂
  injection h₂

/-- O2: A terminal turn state hides the overlay. -/
theorem projectActiveOverlay_terminal_hides
    (resp : Option ResponseSnapshot)
    (t : ClientTurnState)
    (h : t.isTerminal = true)
    (materialized hasContent hasReasoning : Bool) :
    projectActiveOverlay resp (some t) materialized hasContent hasReasoning = none := by
  cases resp with
  | none => rfl
  | some r =>
    cases materialized with
    | true =>
      simp [projectActiveOverlay]
    | false =>
      cases r with
      | mk status tail =>
        cases status with
        | streaming =>
          cases t with
          | waitingForClaim => simp [ClientTurnState.isTerminal] at h
          | streaming       => simp [ClientTurnState.isTerminal] at h
          | completed       => simp [projectActiveOverlay, ClientTurnState.isTerminal]
          | failed          => simp [projectActiveOverlay, ClientTurnState.isTerminal]
          | superseded      => simp [projectActiveOverlay, ClientTurnState.isTerminal]
          | interrupted     => simp [projectActiveOverlay, ClientTurnState.isTerminal]
        | complete =>
          simp [projectActiveOverlay]
        | error =>
          simp [projectActiveOverlay]

/-- O3: A materialized response hides the overlay. -/
theorem projectActiveOverlay_materialized_hides
    (resp : Option ResponseSnapshot)
    (turn : Option ClientTurnState)
    (hasContent hasReasoning : Bool) :
    projectActiveOverlay resp turn true hasContent hasReasoning = none := by
  cases resp with
  | none => rfl
  | some _ => rfl
