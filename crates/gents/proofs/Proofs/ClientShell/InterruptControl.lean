import Proofs.ClientShell.SessionTurn
import Proofs.ClientShell.Submission

namespace ClientShell.InterruptControl

inductive Phase where
  | idle | submitting | awaiting | active | blocked
  deriving DecidableEq, Repr

def phaseOf (workflow : SubmissionWorkflow) (decision : SendDecision) : Phase :=
  match workflow with
  | .submitting _ _ => .submitting
  | .awaiting _ _ => .awaiting
  | .blocked _ => .blocked
  | .idle => match decision with
    | .ready => .idle
    | .queue _ => .active
    | .blocked _ => .blocked

def visible : Phase → Bool
  | .awaiting | .active => true
  | _ => false

def target (tracked latest : Option RequestId) (turn : Option ClientTurnState) :
    Option RequestId :=
  match latest, turn with
  | some request, some state => if state.isTerminal then tracked else some request
  | _, _ => tracked

def stopping (inFlight : Bool) (request latest : Option RequestId)
    (interruptObserved : Bool) (requested : Option RequestId) : Bool :=
  if !inFlight then false else
  match request with
  | none => false
  | some id => requested == some id || (latest == some id && interruptObserved)

theorem active_target_wins (tracked : Option RequestId) (request : RequestId)
    (turn : ClientTurnState) (h : turn.isTerminal = false) :
    target tracked (some request) (some turn) = some request := by
  simp [target, h]

theorem terminal_target_uses_tracked (tracked : Option RequestId) (request : RequestId)
    (turn : ClientTurnState) (h : turn.isTerminal = true) :
    target tracked (some request) (some turn) = tracked := by
  simp [target, h]

theorem inactive_clears_stopping (request latest requested : Option RequestId)
    (interruptObserved : Bool) :
    stopping false request latest interruptObserved requested = false := by
  simp [stopping]

theorem previous_stop_does_not_mark_new_owner (request old : RequestId)
    (latest : Option RequestId) (h : request ≠ old) :
    stopping true (some request) latest false (some old) = false := by
  simp [stopping, h, Ne.symm h]

theorem displayed_active_owner_is_interrupt_target (sid : SessionId) (node : NodeDid)
    (agent : Option AgentId) (rows : List SessionTurn.Row) (selected owner : SessionTurn.Row)
    (tracked : Option RequestId)
    (hterminal : (SessionTurn.turnOf rows (SessionTurn.replacementOf rows selected)).state = .terminal)
    (howner : ((SessionTurn.displayCandidates rows selected).reverse.find?
      (fun row => (row.requester == selected.requester && row.publicInput) &&
        row.state == .active)) = some owner) :
    target tracked (SessionTurn.observe sid node agent rows selected).latestObservedRequest
      (SessionTurn.observe sid node agent rows selected).latestTurn = some owner.request := by
  obtain ⟨hrequest, hturn, _⟩ :=
    SessionTurn.active_owner_remains_observable sid node agent rows selected owner hterminal howner
  simp [hrequest, hturn, target, ClientTurnState.isTerminal]

theorem observed_running_is_interruptible
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext)
    (sid : SessionId) (obs : SessionObservation) (request : RequestId)
    (tracked : Option RequestId)
    (hclient : ctx.clientAvailable = true)
    (hnode : s.selection.node.isSome = true)
    (hw : s.workflow = .idle)
    (hsel : s.selection.session = some sid)
    (hfind : store.find sid = some obs)
    (hagent : agentMismatch store sid ctx.requestedAgent = false)
    (hrequest : obs.latestObservedRequest = some request)
    (hturn : obs.latestTurn = some .running) :
    visible (phaseOf s.workflow (projectSendDecision s store ctx)) = true ∧
    target tracked obs.latestObservedRequest obs.latestTurn = some request := by
  have hqueue := nonterminal_turn_queues s store ctx sid obs request .running
    hclient hnode hw hsel hfind hagent hrequest hturn (by rfl)
  simp [phaseOf, hw, hqueue, visible, target, hrequest, hturn, ClientTurnState.isTerminal]

end ClientShell.InterruptControl
