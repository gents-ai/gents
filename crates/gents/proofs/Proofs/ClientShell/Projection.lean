import Proofs.ClientShell.Transition

inductive SelectionHealth where
  | noSelection
  | resolved
  | pendingObservation
  | absent
  deriving DecidableEq, Repr

structure ChatView where
  selection       : Selection
  selectionHealth : SelectionHealth
  visibleSession  : Option SessionObservation
  turnState       : Option ClientTurnState
  workflow        : SubmissionWorkflow
  sendDecision    : SendDecision
  deriving DecidableEq, Repr

def workflowReferences (w : SubmissionWorkflow) (sid : SessionId) : Bool :=
  match w with
  | .submitting _ (some sid') => decide (sid = sid')
  | .awaiting sid' _          => decide (sid = sid')
  | _                         => false

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

def projectChat
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : ChatView :=
  let classified := classifySelection s.selection store s.workflow
  { selection       := s.selection,
    selectionHealth := classified.fst,
    visibleSession  := classified.snd,
    turnState       := classified.snd.bind (·.latestTurn),
    workflow        := s.workflow,
    sendDecision    := projectSendDecision s store ctx }

inductive TransportIndicator where
  | quiet
  | degradedNotice
  | wedgedNotice
  deriving DecidableEq, Repr

def projectTransportIndicator : TransportHealth → TransportIndicator
  | .healthy  => .quiet
  | .degraded => .degradedNotice
  | .wedged   => .wedgedNotice

inductive RecoveryStatusKind where
  | ready
  | waiting
  | blocked
  deriving DecidableEq, Repr

/-- Transport and route readiness are observations of automatic native recovery.
Enrolled peers are reconciled automatically by the existing native owner.
Waiting neither promises a recovery deadline nor grants enrollment authority.
These observations do not establish that a user command is required; explicit
credential or configuration errors retain their separate action owners. -/
structure RecoveryStatus where
  kind : RecoveryStatusKind
  action : Option Unit
  deriving DecidableEq, Repr

def projectTransportRecovery (connected : Bool) : RecoveryStatus :=
  { kind := if connected then .ready else .waiting, action := none }

def projectRouteRecovery (routeReady : Bool) (_pairingPending : Bool) : RecoveryStatus :=
  { kind := if routeReady then .ready else .waiting, action := none }

/-- Failed P2P observations remain visible blockers. Recovery is automatic;
operator remediation for credentials or configuration belongs to its own action. -/
def projectConnectionFailure : RecoveryStatus :=
  { kind := .blocked, action := none }

theorem connection_failure_preserves_block_without_reconnect :
    projectConnectionFailure.kind = .blocked ∧ projectConnectionFailure.action = none := by
  exact ⟨rfl, rfl⟩

theorem transport_recovery_requires_no_action (connected : Bool) :
    (projectTransportRecovery connected).action = none := by
  rfl

theorem route_recovery_requires_no_action (ready pairingPending : Bool) :
    (projectRouteRecovery ready pairingPending).action = none := by
  rfl

theorem offline_transport_waits :
    (projectTransportRecovery false).kind = .waiting := by
  rfl

theorem unready_route_waits (pairingPending : Bool) :
    (projectRouteRecovery false pairingPending).kind = .waiting := by
  rfl

/-- Request progress is projected directly from the persisted request
lifecycle. Clients may choose presentation, but must not collapse the active
states into one generic spinner. -/
inductive RequestProgressIndicator where
  | queued
  | claimed
  | working
  | completed
  | failed
  | superseded
  | expired
  | interrupted
  deriving DecidableEq, Repr

def projectRequestProgress : RequestState → RequestProgressIndicator
  | .workspaceBindingPending => .queued
  | .pending       => .queued
  | .claimed       => .claimed
  | .processing    => .working
  | .completed     => .completed
  | .failed        => .failed
  | .superseded    => .superseded
  | .dead          => .expired
  | .interrupted   => .interrupted

def RequestProgressIndicator.label : RequestProgressIndicator → String
  | .queued          => "Queued"
  | .claimed         => "Claimed"
  | .working         => "Working"
  | .completed       => "Completed"
  | .failed          => "Failed"
  | .superseded      => "Superseded"
  | .expired         => "Expired"
  | .interrupted     => "Interrupted"

def RequestProgressIndicator.animated : RequestProgressIndicator → Bool
  | .queued | .claimed | .working => true
  | _ => false

theorem projectRequestProgress_active_animated (state : RequestState)
    (h : state = .pending ∨ state = .claimed ∨ state = .processing) :
    (projectRequestProgress state).animated = true := by
  rcases h with rfl | rfl | rfl <;> rfl

/-- The request document owns the pending user projection until the durable
user message for that exact request arrives. Unrelated messages and their
relative replication order are deliberately irrelevant. -/
def projectPendingUserTurn (hasDurableUserOwner : Bool) : Bool :=
  !hasDurableUserOwner

theorem projectPendingUserTurn_without_owner_visible :
    projectPendingUserTurn false = true := by
  rfl

theorem projectPendingUserTurn_with_owner_hidden :
    projectPendingUserTurn true = false := by
  rfl

structure OverlayBlock where
  hasContent   : Bool
  hasReasoning : Bool
  deriving DecidableEq, Repr

def projectActiveOverlay
    (liveOutputAvailable : Bool)
    (turn : Option ClientTurnState)
    (hasDurableOwner : Bool)
    (hasContent hasReasoning : Bool)
    : Option OverlayBlock :=
  if !liveOutputAvailable then none
  else if hasDurableOwner then none
  else
    match turn with
    | none => none
    | some t =>
      if t.isTerminal then none
      else if t = .waitingForClaim ∨ t = .running then
        if hasContent ∨ hasReasoning then
          some { hasContent := hasContent, hasReasoning := hasReasoning }
        else none
      else none

theorem projectActiveOverlay_at_most_one
    (liveOutputAvailable : Bool)
    (turn : Option ClientTurnState)
    (hasDurableOwner hasContent hasReasoning : Bool) :
    ∀ b₁ b₂,
      projectActiveOverlay liveOutputAvailable turn hasDurableOwner hasContent hasReasoning = some b₁ →
      projectActiveOverlay liveOutputAvailable turn hasDurableOwner hasContent hasReasoning = some b₂ →
      b₁ = b₂ := by
  intros b₁ b₂ h₁ h₂
  rw [h₁] at h₂
  injection h₂

theorem projectActiveOverlay_terminal_hides
    (liveOutputAvailable : Bool)
    (t : ClientTurnState)
    (h : t.isTerminal = true)
    (hasDurableOwner hasContent hasReasoning : Bool) :
    projectActiveOverlay liveOutputAvailable (some t) hasDurableOwner
      hasContent hasReasoning = none := by
  cases liveOutputAvailable <;> cases hasDurableOwner <;>
    simp [projectActiveOverlay, h]

/-- A live preview is hidden once the same request already has a durable
assistant turn owning that content. -/
theorem projectActiveOverlay_durable_owner_hides
    (liveOutputAvailable : Bool)
    (turn : Option ClientTurnState)
    (hasContent hasReasoning : Bool) :
    projectActiveOverlay liveOutputAvailable turn true hasContent hasReasoning = none := by
  cases liveOutputAvailable <;> rfl
