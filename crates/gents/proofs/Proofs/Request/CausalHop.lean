import Proofs.Basic

/-!
# Causal hop: the only loop bound between agents

Every agent is an ordinary agent, addressed directly; the runtime encodes no
parent/child hierarchy. The one causal fact the runtime keeps is how many
tool-caused requests separate a request from its root. It is the signed,
immutable `AgentRequest.subagent_depth`, written once by
`lifecycle::materialize` and checked at admission against the target's
`AgentPrincipal.max_request_hop`. Agents that message each other in a cycle
are therefore refused after a bounded number of sends, with no cascade,
fence or tree walk.
-/

namespace CausalHop

/-- Default `AgentPrincipal.max_request_hop` when the principal leaves it unset. -/
def defaultMaxRequestHop : Nat := 8

/-- Why a request exists, relative to the request whose hop it inherits. -/
inductive Cause where
  /-- A user, trigger or schedule root. -/
  | root
  /-- `create_session`/`send_message` materialized a new request in another
  session, naming the calling request and tool call as its lineage. -/
  | toolCall
  /-- Continuation of the same work: retry, goal continuation, steering or a
  completion wake. It is not a new send, so it cannot extend a chain. -/
  | continuation
  deriving DecidableEq, Repr

/-- The hop a request is materialized with, given its predecessor's hop.
Roots start at zero, a tool-caused request is one further than the request
that caused it, and a continuation copies its predecessor's hop. -/
def nextHop : Cause → Nat → Nat
  | .root, _ => 0
  | .toolCall, predecessor => predecessor + 1
  | .continuation, predecessor => predecessor

/-- Admission refuses a request whose hop exceeds the target principal's
`max_request_hop`. The check reads only the signed hop; no lineage walk,
replication of the predecessor, or cooperation of the sender is needed. -/
def admitHop (maxHop hop : Nat) : Bool := decide (hop ≤ maxHop)

theorem root_hop_is_zero (predecessor : Nat) : nextHop .root predecessor = 0 := rfl

theorem continuation_preserves_hop (predecessor : Nat) :
    nextHop .continuation predecessor = predecessor := rfl

theorem tool_caused_request_is_one_further (predecessor : Nat) :
    nextHop .toolCall predecessor = predecessor + 1 := rfl

/-- The hop reached after a sequence of causes from a starting hop. -/
def hopAlong (start : Nat) : List Cause → Nat
  | [] => start
  | cause :: rest => hopAlong (nextHop cause start) rest

/-- Number of sends in a chain. -/
def sends (causes : List Cause) : Nat :=
  (causes.filter (· == .toolCall)).length

theorem hopAlong_without_root (start : Nat) (causes : List Cause)
    (h : Cause.root ∉ causes) :
    hopAlong start causes = start + sends causes := by
  induction causes generalizing start with
  | nil => simp [hopAlong, sends]
  | cons cause rest ih =>
    have h_rest : Cause.root ∉ rest := by intro hm; apply h; simp [hm]
    have h_head : cause ≠ .root := by intro he; apply h; simp [he]
    cases cause with
    | root => exact absurd rfl h_head
    | toolCall =>
      simp only [hopAlong, nextHop, ih _ h_rest, sends, List.filter_cons]
      simp [sends]
      omega
    | continuation =>
      simp only [hopAlong, nextHop, ih _ h_rest, sends, List.filter_cons]
      simp

/-- Every admitted chain is bounded: a request admitted at the end of a chain
of causes from a root has at most `maxHop` sends behind it. -/
theorem admitted_chain_sends_le_max (maxHop : Nat) (causes : List Cause)
    (h_rooted : Cause.root ∉ causes)
    (h_admit : admitHop maxHop (hopAlong 0 causes) = true) :
    sends causes ≤ maxHop := by
  rw [hopAlong_without_root 0 causes h_rooted] at h_admit
  simpa [admitHop] using h_admit

/-- A message loop is cut: the send that would exceed the bound is refused. -/
theorem send_beyond_max_is_refused (maxHop : Nat) (causes : List Cause)
    (h_rooted : Cause.root ∉ causes)
    (h_sends : sends causes = maxHop + 1) :
    admitHop maxHop (hopAlong 0 causes) = false := by
  rw [hopAlong_without_root 0 causes h_rooted, h_sends]
  simp [admitHop]

/-- Continuations never change admissibility: steering, retries, goal
continuations and completion wakes of an admitted request stay admitted. -/
theorem continuation_preserves_admission (maxHop hop : Nat) :
    admitHop maxHop (nextHop .continuation hop) = admitHop maxHop hop := rfl

end CausalHop
