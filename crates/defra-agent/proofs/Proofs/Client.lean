import Proofs.Request

/-!
# Client Turn Observation

Formal model for how any client derives a deterministic view of a single
agent turn from observed documents.

The client projection is a pure function of document snapshots. It does
not depend on wall-clock time — server liveness proofs (L1, L3) guarantee
every request terminates. If a client perceives a "stall," that is a
transport problem, not a turn-state problem.

Imports `Proofs.Request` to reuse `RequestState` from the server model.
-/

/-- The 5 client-visible turn states. -/
inductive ClientTurnState where
  | waitingForClaim
  | streaming
  | completed
  | failed
  | superseded
  deriving DecidableEq, Repr

namespace ClientTurnState

/-- Client state ordering for monotonicity.
    Terminal states share rank 2 (incomparable). -/
def rank : ClientTurnState → Nat
  | .waitingForClaim => 0
  | .streaming       => 1
  | .completed       => 2
  | .failed          => 2
  | .superseded      => 2

/-- Whether a client turn state is terminal. -/
def isTerminal : ClientTurnState → Bool
  | .completed  => true
  | .failed     => true
  | .superseded => true
  | _           => false

instance : HasTerminal ClientTurnState where
  isTerminal s := s.isTerminal = true
  isTerminal_dec s := by
    cases s <;> simp [isTerminal] <;> infer_instance

end ClientTurnState

/-- Client-visible response status, read from AgentResponse.status. -/
inductive ResponseStatus where
  | streaming
  | complete
  | error
  deriving DecidableEq, Repr

/-- Snapshot of an AgentRequest as observed by the client.
    Only the fields that affect derivation are included. -/
structure RequestSnapshot where
  lifecycleState : RequestState
  isSuperseded : Bool
  deriving DecidableEq, Repr

/-- Snapshot of an AgentResponse as observed by the client.
    progressSeq is omitted — it orders response versions
    but does not affect the derivation result. -/
structure ResponseSnapshot where
  status : ResponseStatus
  deriving DecidableEq, Repr

/-- A single attempt observation: request + optional response. -/
structure AttemptView where
  request : RequestSnapshot
  response : Option ResponseSnapshot
  deriving DecidableEq, Repr
