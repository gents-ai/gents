import Proofs.Client

/-!
# Client Shell Types

Shell state, observations, selection state, and input vocabulary above client turn projection.
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
