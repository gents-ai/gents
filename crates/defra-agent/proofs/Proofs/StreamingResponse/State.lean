import Proofs.Basic
import Proofs.Persistence
import Proofs.Request.State
import Proofs.Transcript.State

/-!
# StreamingResponse State

State vocabulary for the AgentResponse streaming → terminal lifecycle.
See `docs/superpowers/specs/2026-05-14-agent-response-streaming-lean-design.md`.
-/

namespace StreamingResponse

abbrev DocId := Nat

inductive Status where
  | streaming
  | completed
  | error
  deriving DecidableEq, Repr

namespace Status

def toDefraDB : Status → String
  | .streaming => "streaming"
  | .completed => "complete"
  | .error => "error"

def fromDefraDB? : String → Option Status
  | "streaming" => some .streaming
  | "complete" => some .completed
  | "error" => some .error
  | _ => none

theorem fromDefraDB_toDefraDB (s : Status) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

instance : HasTerminal Status where
  isTerminal s := s = .completed ∨ s = .error
  isTerminal_dec s :=
    match s with
    | .completed => isTrue (Or.inl rfl)
    | .error => isTrue (Or.inr rfl)
    | .streaming => isFalse (by
        intro h
        cases h with
        | inl h => cases h
        | inr h => cases h)

end Status

inductive ErrorReason where
  | streamIdleTimeout
  | daemonRestartRecovery
  | inferenceFailed
  | finalizeRequestedError
  | interrupted
  deriving DecidableEq, Repr

namespace ErrorReason

def toContract : ErrorReason → String
  | .streamIdleTimeout => "streamIdleTimeout"
  | .daemonRestartRecovery => "daemonRestartRecovery"
  | .inferenceFailed => "inferenceFailed"
  | .finalizeRequestedError => "finalizeRequestedError"
  | .interrupted => "interrupted"

end ErrorReason

inductive LiveTail where
  | empty
  | nonEmpty
  deriving DecidableEq, Repr

namespace LiveTail

def toContract : LiveTail → String
  | .empty => "empty"
  | .nonEmpty => "nonEmpty"

end LiveTail

end StreamingResponse
