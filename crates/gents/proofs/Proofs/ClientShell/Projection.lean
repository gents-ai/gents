import Proofs.ClientShell.Transition
import Proofs.ClientShell.ChatReadiness
import Proofs.ClientShell.SyncHealth

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

/-- The chat write route follows the chat write fence alone
(`ClientChatReadiness`): no refusal is ready, a refusal that awaits the
runtime's advertisement waits, and one whose next action is connecting the
runtime is blocked. Pairing is not an input. -/
def projectRouteRecovery : Option ClientChatReadiness.Cause → RecoveryStatus
  | none => { kind := .ready, action := none }
  | some .endpointNotAdvertised => { kind := .waiting, action := none }
  | some _ => { kind := .blocked, action := none }

def projectConnectionFailure : RecoveryStatus :=
  { kind := .blocked, action := none }

/-- The sync surface shows `ClientSyncHealth.view`'s state unchanged: healthy is
ready, and waiting and blocked keep their tone. The view's own next action names
what the user does, so the status adds no recovery action. -/
def projectSyncRecovery (view : ClientSyncHealth.View) : RecoveryStatus :=
  { kind := match view.state with
      | .healthy => .ready
      | .waiting => .waiting
      | .blocked => .blocked
    action := none }

theorem sync_recovery_requires_no_action (view : ClientSyncHealth.View) :
    (projectSyncRecovery view).action = none := by
  rfl

theorem connection_failure_preserves_block_without_reconnect :
    projectConnectionFailure.kind = .blocked ∧ projectConnectionFailure.action = none := by
  exact ⟨rfl, rfl⟩

theorem transport_recovery_requires_no_action (connected : Bool) :
    (projectTransportRecovery connected).action = none := by
  rfl

theorem route_recovery_requires_no_action (refusal : Option ClientChatReadiness.Cause) :
    (projectRouteRecovery refusal).action = none := by
  cases refusal with
  | none => rfl
  | some cause => cases cause <;> rfl

theorem offline_transport_waits :
    (projectTransportRecovery false).kind = .waiting := by
  rfl

/-- The route is ready exactly when the fence admits the write. -/
theorem route_ready_iff_fence_admits (route : Option ClientChatReadiness.Route) :
    (projectRouteRecovery (match ClientChatReadiness.chatReadiness route with
      | .ready => none | .notReady cause => some cause)).kind = .ready ↔
      ClientChatReadiness.chatReadiness route = .ready := by
  cases h : ClientChatReadiness.chatReadiness route with
  | ready => simp [projectRouteRecovery]
  | notReady cause => cases cause <;> simp [projectRouteRecovery]

/-- Request progress is projected directly from the persisted request
lifecycle. Clients may choose presentation, but must not collapse the active
states into one generic spinner. -/
inductive RequestProgressIndicator where
  | queued
  | claimed
  | working
  | waitingForBackend
  | resumingBackend
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
  | .waitingForBackend => .waitingForBackend
  | .resumingBackend => .resumingBackend
  | .completed     => .completed
  | .failed        => .failed
  | .superseded    => .superseded
  | .dead          => .expired
  | .interrupted   => .interrupted

def RequestProgressIndicator.label : RequestProgressIndicator → String
  | .queued          => "Queued"
  | .claimed         => "Claimed"
  | .working         => "Working"
  | .waitingForBackend => "Waiting for provider"
  | .resumingBackend => "Reconnecting to provider"
  | .completed       => "Completed"
  | .failed          => "Failed"
  | .superseded      => "Superseded"
  | .expired         => "Expired"
  | .interrupted     => "Stopped"

def RequestProgressIndicator.animated : RequestProgressIndicator → Bool
  | .queued | .claimed | .working | .waitingForBackend | .resumingBackend => true
  | _ => false

theorem projectRequestProgress_active_animated (state : RequestState)
    (h : state = .pending ∨ state = .claimed ∨ state = .processing) :
    (projectRequestProgress state).animated = true := by
  rcases h with rfl | rfl | rfl <;> rfl

/-- The request document owns the pending user projection until the durable
user message for that exact request arrives. Unrelated messages and their
relative replication order are deliberately irrelevant.

A request its author retired from the queue (`Session.replacePendingGroup?`:
removed, or replaced by an edit or a move) is never shown. It was never
delivered, so no durable message will ever own it, and what the author meant
is its replacement or nothing; showing it would put text in the transcript
that the agent never read. -/
def projectPendingUserTurn (hasDurableUserOwner : Bool) (retiredByAuthor : Bool := false) :
    Bool :=
  !hasDurableUserOwner && !retiredByAuthor

theorem projectPendingUserTurn_without_owner_visible :
    projectPendingUserTurn false = true := by
  rfl

theorem projectPendingUserTurn_with_owner_hidden :
    projectPendingUserTurn true = false := by
  rfl

theorem projectPendingUserTurn_retired_hidden (hasDurableUserOwner : Bool) :
    projectPendingUserTurn hasDurableUserOwner true = false := by
  cases hasDurableUserOwner <;> rfl

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

theorem projectActiveOverlay_durable_owner_hides
    (liveOutputAvailable : Bool)
    (turn : Option ClientTurnState)
    (hasContent hasReasoning : Bool) :
    projectActiveOverlay liveOutputAvailable turn true hasContent hasReasoning = none := by
  cases liveOutputAvailable <;> rfl
