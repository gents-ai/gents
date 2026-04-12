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

/-- Derive client turn state from a single attempt observation.

    Priority order:
    1. Supersession takes absolute precedence (cross-turn event).
    2. Server terminal lifecycle states override any response
       (terminal states are irreversible — proven in Request.lean).
    3. For non-terminal request states, response may be more current
       than the request under P2P replication lag. Trust the response.
    4. No response and non-terminal request → waitingForClaim. -/
def deriveAttempt : AttemptView → ClientTurnState
  | ⟨req, resp⟩ =>
    -- Supersession: cross-turn event, always takes precedence
    if req.isSuperseded then .superseded
    else match req.lifecycleState with
    -- Server terminal states override any stale response
    | .superseded    => .superseded
    | .completed     => .completed
    | .failed        => .failed
    | .dead          => .failed
    -- Non-terminal: response may be more current than request
    | .pending | .claimed | .processing | .inputRequired =>
      match resp with
      | some r =>
        match r.status with
        | .complete  => .completed
        | .error     => .failed
        | .streaming => .streaming
      | none => .waitingForClaim

/-- Derive client turn state from a full turn observation.

    The turn is a retry chain: a list of attempts ordered root-first,
    tip-last. The tip is the most recent attempt — the one the client
    should render.

    Returns `none` for empty observations (no turn exists). -/
def deriveTurn : List AttemptView → Option ClientTurnState
  | []          => none
  | [a]         => some (deriveAttempt a)
  | _ :: rest   => deriveTurn rest

/-- deriveTurn always returns the derivation of the last element. -/
theorem deriveTurn_append_singleton
    (attempts : List AttemptView)
    (a : AttemptView) :
    deriveTurn (attempts ++ [a]) = some (deriveAttempt a) := by
  induction attempts with
  | nil => rfl
  | cons head tail ih =>
    cases tail with
    | nil => rfl
    | cons h' t' =>
      -- Now (head :: h' :: t') ++ [a] = head :: (h' :: t' ++ [a])
      -- and deriveTurn matches the `_ :: rest` case
      simp only [List.cons_append, deriveTurn]
      exact ih
