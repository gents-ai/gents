import Proofs.Request

inductive ClientTurnState where
  | waitingForClaim
  | running
  | completed
  | failed
  | superseded
  | interrupted
  deriving DecidableEq, Repr

namespace ClientTurnState

def rank : ClientTurnState → Nat
  | .waitingForClaim => 0
  | .running         => 1
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

structure RequestSnapshot where
  lifecycleState : RequestState
  isSuperseded : Bool
  deriving DecidableEq, Repr

structure AttemptView where
  request : RequestSnapshot
  deriving DecidableEq, Repr

/-- `observationScope` names the caller's already authorized agent/requester/
session observation scope, not a request document ID: every retry has its own
physical request document. Lineage and graph validity remain premises
established by the owner. -/
structure KeyedAttemptView where
  observationScope : String
  requestId : String
  retryParentRequest : Option String
  attempt : AttemptView
  deriving DecidableEq, Repr

def deriveAttempt : AttemptView → ClientTurnState
  | ⟨req⟩ =>
    if req.isSuperseded then .superseded
    else match req.lifecycleState with
    | .superseded    => .superseded
    | .completed     => .completed
    | .failed        => .failed
    | .dead          => .failed
    | .interrupted   => .interrupted
    | .workspaceBindingPending | .pending => .waitingForClaim
    | .claimed | .processing => .running

structure ClientHeadProjection where
  turnState : ClientTurnState
  requestState : RequestState
  deriving DecidableEq, Repr

def projectHead (view : AttemptView) : ClientHeadProjection :=
  { turnState := deriveAttempt view
  , requestState := view.request.lifecycleState
  }

def ClientHeadProjection.isTerminal (head : ClientHeadProjection) : Bool :=
  head.turnState.isTerminal

def ClientHeadProjection.isActive (head : ClientHeadProjection) : Bool :=
  !head.isTerminal

/-- `attempts` is an already validated retry chain in oldest-to-newest order;
selection from that ordered chain only, not store-level retry-tip uniqueness. -/
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

def attemptsInObservationScope
    (observationScope : String)
    (attempts : List KeyedAttemptView) : List KeyedAttemptView :=
  attempts.filter fun attempt => attempt.observationScope == observationScope

def retryParentIds (attempts : List KeyedAttemptView) : List String :=
  attempts.filterMap fun attempt => attempt.retryParentRequest

def retryTips
    (observationScope : String)
    (attempts : List KeyedAttemptView) : List KeyedAttemptView :=
  let scopedAttempts := attemptsInObservationScope observationScope attempts
  let parents := retryParentIds scopedAttempts
  scopedAttempts.filter fun attempt => !(parents.contains attempt.requestId)

/-- Resolve an unordered observation set only when its parent-ID filter has one
candidate tip; zero-tip sets (including a closed cycle) and multiple-tip
ambiguity are rejected. Graph topology is not independently validated here. -/
def resolveRetryTip
    (observationScope : String)
    (attempts : List KeyedAttemptView) : Option KeyedAttemptView :=
  match retryTips observationScope attempts with
  | [tip] => some tip
  | _ => none

def deriveUnorderedTurn
    (observationScope : String)
    (attempts : List KeyedAttemptView) : Option ClientTurnState :=
  (resolveRetryTip observationScope attempts).map fun tip => deriveAttempt tip.attempt
