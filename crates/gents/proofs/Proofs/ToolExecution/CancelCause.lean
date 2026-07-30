import Proofs.Basic

namespace ToolExecution

inductive CancelCause where
  | interrupted
  | deadline
  | userCancelled
  deriving DecidableEq, Repr

namespace CancelCause

def toDefraDB : CancelCause → String
  | .interrupted => "interrupted"
  | .deadline => "deadline"
  | .userCancelled => "userCancelled"

def fromDefraDB? : String → Option CancelCause
  | "interrupted" => some .interrupted
  | "deadline" => some .deadline
  | "userCancelled" => some .userCancelled
  | _ => none

theorem fromDefraDB_toDefraDB (cause : CancelCause) :
    fromDefraDB? cause.toDefraDB = some cause := by
  cases cause <;> rfl

def all : List CancelCause :=
  [ .interrupted, .deadline, .userCancelled ]

theorem all_complete (cause : CancelCause) : cause ∈ all := by
  cases cause <;> simp [all]

end CancelCause
end ToolExecution
