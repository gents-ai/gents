import Proofs.Basic
import Proofs.Triggers.Groups

inductive InvocationState where
  | pending
  | claimed
  | running
  | succeeded
  | failed
  | denied
  deriving DecidableEq, Repr

namespace InvocationState

def toDefraDB : InvocationState → String
  | .pending => "pending"
  | .claimed => "claimed"
  | .running => "running"
  | .succeeded => "succeeded"
  | .failed => "failed"
  | .denied => "denied"

def fromDefraDB? : String → Option InvocationState
  | "pending" => some .pending
  | "claimed" => some .claimed
  | "running" => some .running
  | "succeeded" => some .succeeded
  | "failed" => some .failed
  | "denied" => some .denied
  | _ => none

theorem fromDefraDB_toDefraDB (s : InvocationState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

instance : HasTerminal InvocationState where
  isTerminal s := s = .succeeded ∨ s = .failed ∨ s = .denied
  isTerminal_dec s :=
    match s with
    | .succeeded => isTrue (Or.inl rfl)
    | .failed => isTrue (Or.inr (Or.inl rfl))
    | .denied => isTrue (Or.inr (Or.inr rfl))
    | .pending => isFalse (by
        intro h
        cases h with
        | inl h => cases h
        | inr h =>
            cases h with
            | inl h => cases h
            | inr h => cases h)
    | .claimed => isFalse (by
        intro h
        cases h with
        | inl h => cases h
        | inr h =>
            cases h with
            | inl h => cases h
            | inr h => cases h)
    | .running => isFalse (by
        intro h
        cases h with
        | inl h => cases h
        | inr h =>
            cases h with
            | inl h => cases h
            | inr h => cases h)

end InvocationState

inductive ActionJournalState where
  | validated
  | executing
  | effectObserved
  | resultDocsWritten
  /-- Recovery found the action still executing: what it did outside the
  runtime is unknown. -/
  | interrupted
  deriving DecidableEq, Repr

namespace ActionJournalState

def toDefraDB : ActionJournalState → String
  | .validated => "validated"
  | .executing => "executing"
  | .effectObserved => "effectObserved"
  | .resultDocsWritten => "resultDocsWritten"
  | .interrupted => "interrupted"

def fromDefraDB? : String → Option ActionJournalState
  | "validated" => some .validated
  | "executing" => some .executing
  | "effectObserved" => some .effectObserved
  | "resultDocsWritten" => some .resultDocsWritten
  | "interrupted" => some .interrupted
  | _ => none

theorem fromDefraDB_toDefraDB (s : ActionJournalState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

def laterThanValidated : ActionJournalState → Bool
  | .validated => false
  | .executing | .effectObserved | .resultDocsWritten | .interrupted => true

/-- The action's effect happened, its results were written, or it was
interrupted with an unknown outcome; running it again could repeat that effect. -/
def effectful : ActionJournalState → Bool
  | .effectObserved | .resultDocsWritten | .interrupted => true
  | .validated | .executing => false

def markInterrupted : ActionJournalState → ActionJournalState
  | .executing => .interrupted
  | s => s

end ActionJournalState

structure ActionJournalEntry where
  index : Nat
  state : ActionJournalState
  deriving DecidableEq, Repr

structure CallbackInvocation where
  invocationId : String
  ownerNodeDid : String
  /-- Opaque frozen projected input, including ordered group members. Its
  internal JSON encoding belongs to the existing projection owner. -/
  input : String := ""
  originGroupKey : Option Triggers.Groups.EventGroupKey := none
  state : InvocationState
  journal : List ActionJournalEntry
  resultEmitted : Bool
  attempts : Nat := 0
  deriving DecidableEq, Repr
