import Proofs.Request

inductive ClientTurnState where
  | waitingForClaim
  | streaming
  | completed
  | failed
  | superseded
  | interrupted
  deriving DecidableEq, Repr

namespace ClientTurnState

def rank : ClientTurnState → Nat
  | .waitingForClaim => 0
  | .streaming       => 1
  | .completed       => 2
  | .failed          => 2
  | .superseded      => 2
  | .interrupted     => 2

def isTerminal : ClientTurnState → Bool
  | .completed   => true
  | .failed      => true
  | .superseded  => true
  | .interrupted => true
  | _            => false

instance : HasTerminal ClientTurnState where
  isTerminal s := s.isTerminal = true
  isTerminal_dec s := by
    cases s <;> simp [isTerminal] <;> infer_instance

end ClientTurnState

inductive ResponseStatus where
  | streaming
  | complete
  | error
  deriving DecidableEq, Repr

structure RequestSnapshot where
  lifecycleState : RequestState
  isSuperseded : Bool
  deriving DecidableEq, Repr

structure ResponseSnapshot where
  status    : ResponseStatus
  tailEmpty : Bool
  deriving DecidableEq, Repr

structure AttemptView where
  request : RequestSnapshot
  response : Option ResponseSnapshot
  deriving DecidableEq, Repr

def deriveAttempt : AttemptView → ClientTurnState
  | ⟨req, resp⟩ =>
    if req.isSuperseded then .superseded
    else match req.lifecycleState with
    | .superseded    => .superseded
    | .completed     => .completed
    | .failed        => .failed
    | .dead          => .failed
    | .interrupted   => .interrupted
    | .pending | .claimed | .processing | .inputRequired =>
      match resp with
      | some r =>
        match r.status with
        | .complete  => .completed
        | .error     => .failed
        | .streaming => .streaming
      | none => .waitingForClaim

def deriveTurn : List AttemptView → Option ClientTurnState
  | []          => none
  | [a]         => some (deriveAttempt a)
  | _ :: rest   => deriveTurn rest

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
      simp only [List.cons_append, deriveTurn]
      exact ih
