import Proofs.Session.Management

namespace SessionQueue.InputEdit

/-- Facts come from existing signature, enrollment, applied-route and session
owners. A local-self command has no peer route and requires requester=node;
remote commands require the exact fresh enrollment and applied peer route. -/
structure Authority where
  signatureValid : Bool
  replacementsValid : Bool
  sessionOwned : Bool
  localSelf : Bool
  requesterIsNode : Bool
  enrollmentFresh : Bool
  routeApplied : Bool
  /-- All physical command documents have been observed without relying on a unique
  index winner. Distinct documents sharing the command ID fail closed even for identical
  signed intents, and even when
  one already carries a valid receipt; committed effects are never undone. -/
  commandIdentityUnique : Bool := true
  deriving DecidableEq, Repr

structure Command where
  id : Nat
  digest : Nat
  caller : Option Nat
  hasPeer : Bool
  issuedAt : Nat
  expiresAt : Nat
  expected : List RequestId
  offset : Nat
  count : Nat
  replacements : List QueueEntry
  deriving DecidableEq, Repr

inductive Outcome where
  | applied
  | rejected
  deriving DecidableEq, Repr

structure Receipt where
  commandId : Nat
  digest : Nat
  outcome : Outcome
  deriving DecidableEq, Repr

structure State where
  queue : SessionQueueState
  receipts : List Receipt
  deriving DecidableEq

def authorized (a : Authority) (c : Command) (now : Nat) : Bool :=
  a.signatureValid && a.replacementsValid && a.sessionOwned &&
  decide (c.issuedAt ≤ now ∧ now < c.expiresAt) &&
  if c.hasPeer then a.enrollmentFresh && a.routeApplied
  else a.localSelf && a.requesterIsNode

/-- Queue mutation and runtime-signed terminal receipt commit in one transaction.
The digest denotes the full signed intent. Exact replay returns the recorded
outcome without reapplying or reevaluating expiry; ID reuse with different
signed content is rejected without replacing the original receipt. -/
def apply (target : AgentSession.Scope) (now : Nat) (a : Authority)
    (c : Command) (s : State) : State × Option Receipt :=
  if !a.commandIdentityUnique then (s, none) else
  match s.receipts.find? (fun r => r.commandId == c.id) with
  | some receipt => if receipt.digest = c.digest then (s, some receipt) else (s, none)
  | none =>
    let changed := if authorized a c now then
      replacePendingGroup? target s.queue c.caller c.expected c.offset c.count c.replacements
      else none
    let receipt : Receipt := ⟨c.id, c.digest, if changed.isSome then .applied else .rejected⟩
    ({ queue := changed.getD s.queue, receipts := s.receipts ++ [receipt] }, some receipt)

theorem recorded_replay_preserves_state (target : AgentSession.Scope) (now : Nat)
    (a : Authority) (c : Command) (s : State) (r : Receipt)
    (h : s.receipts.find? (fun r => r.commandId == c.id) = some r) :
    (apply target now a c s).1 = s := by
  by_cases hu : a.commandIdentityUnique <;>
    by_cases hd : r.digest = c.digest <;> simp [apply, hu, h, hd]


theorem observed_identity_collision_preserves_state (target : AgentSession.Scope)
    (now : Nat) (a : Authority) (c : Command) (s : State)
    (h : a.commandIdentityUnique = false) :
    apply target now a c s = (s, none) := by
  simp [apply, h]

end SessionQueue.InputEdit
