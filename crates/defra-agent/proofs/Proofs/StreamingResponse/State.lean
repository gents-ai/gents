import Proofs.Basic
import Proofs.Persistence
import Proofs.Request.State
import Proofs.Transcript.State

/-!
# StreamingResponse State

State vocabulary for the AgentResponse streaming → terminal lifecycle.
See `docs/superpowers/specs/2026-05-14-agent-response-streaming-lean-design.md` (removed from the tree; see git history).
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

structure ResponseContext where
  docId                       : DocId
  requestId                   : RequestId
  status                      : Status
  liveTail                    : LiveTail
  /-- Reasoning-presence of the live tail (issue #492 / enable-thinking).
  Tracks whether the streaming live tail currently carries chain-of-thought
  reasoning. This is the source that `finalizeComplete` copies into
  `durableReasoning` at materialize time, before clearing the live tail. -/
  tailReasoning               : LiveTail
  /-- Durable reasoning-presence persisted into the materialized
  `AgentMessage.reasoning` field at finalize/materialize (issue #492). This is
  a NEW, separate persistence captured AT materialize time as a copy of
  `tailReasoning`; it is independent of (and does not relax) the issue #64
  invariant that clears `liveTail` to `.empty` on finalize. -/
  durableReasoning            : LiveTail
  tokenCount                  : Nat
  lastProgressAt              : Time
  streamIdleDeadline          : Time
  now                         : Time
  errorReason                 : Option ErrorReason
  materializedMessageSequence : Option Transcript.Sequence
  interruptedAt               : Option Time
  deriving DecidableEq, Repr

structure ResponseRequestBridge where
  response           : ResponseContext
  requestState       : RequestState
  requestPersistence : PersistenceState
  deriving DecidableEq

end StreamingResponse
