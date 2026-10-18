import Proofs.ClientShell.Types

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

theorem canSubmit_iff_admits (s : ShellState) (store : LocalStore) (ctx : SubmitContext) :
    canSubmit s store ctx = true ↔ (projectSendDecision s store ctx).admits = true := by
  simp [canSubmit]

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
