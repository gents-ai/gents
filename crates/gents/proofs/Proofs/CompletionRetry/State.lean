import Proofs.Basic

namespace CompletionRetry

inductive FailureClass
  | transport
  | parseBadRequest
  | permanent
  deriving DecidableEq, Repr

structure Budget where
  transportRetries : Nat
  resampleRetries : Nat
  allowRepair : Bool
  deriving DecidableEq, Repr

inductive Phase
  | issuing
  | streaming
  | backingOff (wake : Time)
  | repairing
  | turnClosed
  | turnDone
  | exhausted
  | failedPermanent
  deriving DecidableEq, Repr

structure TurnCtx where
  turnIndex : Nat
  effects : Nat
  rendered : Nat
  deriving DecidableEq, Repr

structure State where
  phase : Phase
  budget : Budget
  transportUsed : Nat
  resampleUsed : Nat
  repairUsed : Bool
  lastParseError : Option String
  now : Time
  deadline : Option Time
  turn : TurnCtx
  deriving DecidableEq, Repr

def fitsDeadline (wake : Time) (deadline : Option Time) : Prop :=
  match deadline with
  | none => True
  | some d => wake ≤ d

def State.terminal (s : State) : Prop :=
  s.phase = Phase.turnDone ∨ s.phase = Phase.exhausted ∨ s.phase = Phase.failedPermanent

end CompletionRetry
