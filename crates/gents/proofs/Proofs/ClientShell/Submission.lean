import Proofs.ClientShell.Types

/-- What a submit is decided on beyond the shell's state. The composer's text is
not part of it: the shell decides for content a person would send, and the
composer adds only its own emptiness (`PresentationAgreement.adaptLocalDraft`),
so the shell's decision does not change with each keystroke. -/
structure SubmitContext where
  clientAvailable   : Bool
  requestedAgent : Option AgentId
  deriving Repr

def agentMismatch
    (store : LocalStore) (sid : SessionId)
    (requested : Option AgentId) : Bool :=
  match requested, (store.find sid).bind (·.agentId) with
  | some r, some e => decide (r ≠ e)
  | _, _           => false

inductive SendBlockedReason where
  | clientOffline
  | nodeNotSelected
  | composerEmpty
  | mutationInFlight
  | awaitingObservation
  | sessionAgentMismatch
  | sessionAbsent
  | inconsistentObservation
  | workflowBlocked
  deriving DecidableEq, Repr

/-- `queue` admits a message while the session's turn is not terminal. The
message is queued behind that turn rather than refused: the runtime claims it
afterwards and folds queued user messages from the same requester and settings
into the turn that claims them (`SessionQueue.claimFolding`). -/
inductive SendDecision where
  | ready
  | queue (turn : ClientTurnState)
  | blocked (reason : SendBlockedReason)
  deriving DecidableEq, Repr

def SendDecision.admits : SendDecision → Bool
  | .ready   => true
  | .queue _ => true
  | .blocked _ => false

def projectSendDecision
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : SendDecision :=
  if ¬ ctx.clientAvailable then .blocked .clientOffline
  else if s.selection.node.isNone then .blocked .nodeNotSelected
  else match s.workflow with
    | .submitting _ _ => .blocked .mutationInFlight
    | .awaiting _ _                 => .blocked .awaitingObservation
    | .blocked _                    => .blocked .workflowBlocked
    | .idle =>
      match s.selection.session with
      | none     => .ready
      | some sid =>
        match store.find sid with
        | none     =>
          .blocked .sessionAbsent
        | some obs =>
          if agentMismatch store sid ctx.requestedAgent then
            .blocked .sessionAgentMismatch
          else
            match obs.latestObservedRequest, obs.latestTurn with
            | none,   none   => .ready
            | some _, some t =>
              if t.isTerminal then .ready
              else .queue t
            | _,      _      => .blocked .inconsistentObservation

def canSubmit (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : Bool :=
  (projectSendDecision s store ctx).admits

/-- Submission and its diagnostic projection share exactly one decision owner. -/
theorem canSubmit_iff_admits (s : ShellState) (store : LocalStore) (ctx : SubmitContext) :
    canSubmit s store ctx = true ↔ (projectSendDecision s store ctx).admits = true := by
  simp [canSubmit]

/-- A running or unclaimed turn never refuses a message; it queues it. -/
theorem nonterminal_turn_queues
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (obs : SessionObservation) (req : RequestId) (turn : ClientTurnState)
    (hclient : ctx.clientAvailable = true)
    (hnode : s.selection.node.isSome = true)
    (hw : s.workflow = .idle)
    (hsel : s.selection.session = some sid)
    (hfind : store.find sid = some obs)
    (hagent_match : agentMismatch store sid ctx.requestedAgent = false)
    (hreq : obs.latestObservedRequest = some req)
    (hturn : obs.latestTurn = some turn)
    (hrunning : turn.isTerminal = false) :
    projectSendDecision s store ctx = .queue turn := by
  have hnode' : s.selection.node.isNone = false := by
    cases h : s.selection.node <;> simp_all
  simp [projectSendDecision, hclient, hnode', hw, hsel, hfind, hagent_match, hreq, hturn,
    hrunning]
