import Proofs.Basic

/-!
# Tool Call Cancel Causes

Daemon-visible vocabulary for why a tool call was cancelled. The cause is
carried by cancellation transitions so cross-machine proofs can distinguish
request-interruption, deadline, and explicit operator/user cancellation paths.
-/

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
