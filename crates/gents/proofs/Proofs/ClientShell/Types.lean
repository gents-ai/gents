import Proofs.Client

abbrev PeerId := Nat

abbrev AgentDid := Nat

/-- `latestObservedRequest` is the request of the session's current turn and
`latestTurn` its state. `queuedRequests` are requests admitted behind that
turn and not yet claimed, in queue order. They do not become the current turn
while it is non-terminal: the runtime claims them after it, and a claim folds
the queued user messages directly behind it into the claimed turn
(`SessionQueue.claimFolding`). `foldedRequests` are the requests a claim
folded; each is answered by its claimed request and is never a turn.
`SessionTurn.observe` computes all three from the session's request rows. -/
structure SessionObservation where
  sessionId             : SessionId
  agentDid              : AgentDid
  behaviorId            : Option BehaviorId
  latestObservedRequest : Option RequestId
  latestTurn            : Option ClientTurnState
  queuedRequests        : List RequestId := []
  foldedRequests        : List RequestId := []
  deriving DecidableEq, Repr

/-- Session observations available to the client shell. Transport selection
lives in `Selection` and grants no authority. -/
structure LocalStore where
  sessions    : List SessionObservation
  deriving Repr

namespace LocalStore

def find (store : LocalStore) (sid : SessionId) : Option SessionObservation :=
  store.sessions.find? (fun obs => obs.sessionId == sid)

def hasSession (store : LocalStore) (sid : SessionId) : Bool :=
  (store.find sid).isSome

end LocalStore

inductive TransportHealth where
  | healthy
  | degraded
  | wedged
  deriving DecidableEq, Repr

structure Selection where
  peer    : Option PeerId
  agent   : Option AgentDid
  session : Option SessionId
  deriving DecidableEq, Repr

inductive BlockedReason where
  | clientOffline
  | behaviorMismatch (requested existing : BehaviorId)
  | mutationRejected
  deriving DecidableEq, Repr

inductive SubmissionWorkflow where
  | idle
  | submitting (agent : AgentDid) (session : Option SessionId)
  | awaiting   (session : SessionId) (request : RequestId)
  | blocked    (reason  : BlockedReason)
  deriving DecidableEq, Repr

structure ShellState where
  selection : Selection
  workflow  : SubmissionWorkflow
  deriving DecidableEq, Repr

namespace ShellState

def initial : ShellState :=
  { selection := { peer := none, agent := none, session := none },
    workflow  := .idle }

end ShellState

inductive UserAction where
  | selectPrincipalRoute (peer : PeerId) (agent : AgentDid)
  | selectSession    (session : SessionId)
  | requestNewSession
  | startSubmit
  | acknowledgeBlocker
  deriving DecidableEq, Repr

inductive MutationResult where
  | submitted (session : SessionId) (request : RequestId)
  | failed    (reason  : BlockedReason)
  deriving DecidableEq, Repr

inductive ShellInput where
  | user      (action : UserAction)
  | snapshot  (store  : LocalStore)
  | mutation  (result : MutationResult)
  | transport (health : TransportHealth)
  deriving Repr
