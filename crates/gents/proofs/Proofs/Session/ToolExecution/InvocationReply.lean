import Proofs.Session.ToolExecution.State

/-!
# Invocation reply selection

A tool call's canonical read selects its transcript reply from the session's
`ToolDelivery` publications that name the call's document. Only a user
message whose one block is the call's native `ToolResult` is an invocation
reply. A spawned process never has a native reply of its own and carries at
most one delivery, its completion notification.
-/

namespace ToolExecution.InvocationReply

inductive Read
  | absent
  | reply
  | ambiguous
  | spawnedFabricated
  | deliveryWithoutReply
  deriving DecidableEq, Repr

/-- `deliveries` publications name the call; `replies` of them are native
invocation replies. More than one reply cannot be bound to the call, and a
delivery without a reply is a malformed transcript, never an empty result. -/
def read (deliveries replies : Nat) (spawned : Bool) : Read :=
  if 1 < replies then .ambiguous
  else if spawned && !(replies == 0 && decide (deliveries ≤ 1)) then .spawnedFabricated
  else if replies == 1 then .reply
  else if spawned || deliveries == 0 then .absent
  else .deliveryWithoutReply

def Read.admitted : Read → Bool
  | .absent | .reply => true
  | _ => false

/-- Which stored text a read returns: the reply renders the call's bounded
presentation, and the raw result is the complete output it was bounded from. -/
inductive Source
  | presentation
  | raw
  deriving DecidableEq, Repr

def resultSource (r : Read) : Option Source :=
  if r = .reply then some .presentation else none

def rawSource (r : Read) : Option Source :=
  if r = .reply then some .raw else none

theorem transaction_reader_preserves_raw_beyond_bounded_reply_and_rejects_ambiguity :
    resultSource (read 1 1 false) = some .presentation ∧
      rawSource (read 1 1 false) = some .raw ∧
      read 2 2 false = .ambiguous := by decide

theorem transaction_reader_rejects_tool_delivery_without_native_reply :
    read 1 0 false = .deliveryWithoutReply := by decide

theorem more_than_one_reply_is_never_bound (deliveries replies : Nat) (spawned : Bool)
    (h : 1 < replies) : read deliveries replies spawned = .ambiguous := by
  simp [read, h]

end ToolExecution.InvocationReply
