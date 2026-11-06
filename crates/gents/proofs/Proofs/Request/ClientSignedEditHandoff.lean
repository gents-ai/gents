import Proofs.Request.CrossActorAuthorization

namespace Request.ClientSignedEditHandoff

/-- A replacement's signed native graph contains public blocks and signatures,
not the client's signing key. Its author and physical targets remain bound to
the same input and the immediate requester of the queued edit. -/
structure Graph where
  author : String
  session : String
  agentDocument : String
  inputDigest : String
  evidenceDigest : String
  signatureValid : Bool
  deriving BEq, DecidableEq, Repr

def matches (graph : Graph) (intent : CrossActorAuthorization.Intent) (inputDigest evidenceDigest : String) : Bool :=
  graph.signatureValid && graph.author == intent.author && graph.session == intent.session &&
    graph.agentDocument == intent.agentDocument && graph.inputDigest == inputDigest &&
    graph.evidenceDigest == evidenceDigest

/-- The pinned HTTP signed-create endpoint requires the authenticated
transaction actor to be the graph signer. An Agent actor therefore cannot
use that endpoint to publish a client-authored replacement. -/
def currentHttpAccepts (authenticated graphAuthor : String) : Bool := authenticated == graphAuthor

theorem agent_http_cannot_replay_user_graph : currentHttpAccepts "agent" "user" = false := by rfl

/-- The native mixed-actor transaction verifies the client graph and the
immediate author's permissions, then commits replacement creates, the queue
edit and its receipt together. Unsupported or conflicted native operations
have no effect. -/
structure NativeTransaction where
  mixedActorsSupported : Bool
  requesterChecksCommitted : Bool
  commitSucceeded : Bool
  deriving BEq, DecidableEq, Repr

structure Effect where
  replacement : Bool := false
  queueEdit : Bool := false
  receipt : Bool := false
  deriving BEq, DecidableEq, Repr

def stage (intent : CrossActorAuthorization.Intent) (graph : Graph) (inputDigest evidenceDigest : String)
    (tx : NativeTransaction) : Effect :=
  if tx.mixedActorsSupported && tx.requesterChecksCommitted && tx.commitSucceeded &&
      matches graph intent inputDigest evidenceDigest then ⟨true, true, true⟩ else ⟨false, false, false⟩

theorem unsupported_mixed_actor_is_atomic_noop
    (intent : CrossActorAuthorization.Intent) (graph : Graph) (digest evidence : String)
    (checks commit : Bool) :
    stage intent graph digest evidence ⟨false, checks, commit⟩ = ⟨false, false, false⟩ := by rfl

theorem failed_commit_is_atomic_noop
    (intent : CrossActorAuthorization.Intent) (graph : Graph) (digest evidence : String)
    (supported checks : Bool) :
    stage intent graph digest evidence ⟨supported, checks, false⟩ = ⟨false, false, false⟩ := by
  simp [stage]

theorem mismatched_input_never_publishes
    (intent : CrossActorAuthorization.Intent) (graph : Graph) (digest evidence : String)
    (tx : NativeTransaction) (mismatch : (graph.inputDigest == digest) = false) :
    stage intent graph digest evidence tx = ⟨false, false, false⟩ := by
  simp [stage, matches, mismatch]

theorem changed_evidence_never_publishes
    (intent : CrossActorAuthorization.Intent) (graph : Graph) (digest evidence : String)
    (tx : NativeTransaction) (mismatch : (graph.evidenceDigest == evidence) = false) :
    stage intent graph digest evidence tx = ⟨false, false, false⟩ := by
  simp [stage, matches, mismatch]

/-- A committed receipt is a terminal observation; recovery never creates or
regrants replacement documents after later revocation. -/
def recover (receiptVerified : Bool) (effect : Effect) : Effect :=
  if receiptVerified then ⟨false, false, false⟩ else effect

theorem duplicate_receipt_never_regrants (effect : Effect) :
    recover true effect = ⟨false, false, false⟩ := by rfl

end Request.ClientSignedEditHandoff
