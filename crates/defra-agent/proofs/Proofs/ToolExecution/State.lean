import Proofs.Basic
import Proofs.Persistence
import Proofs.ToolExecution.Policy

/-!
# Tool Call State

Daemon-visible lifecycle vocabulary for an individual tool dispatch. The
lifecycle picks up after `Policy.preflight = .dispatch`; a `.block` decision
skips the lifecycle entirely and persists `failed` at the request level via
the existing `tool_failure_class` field. That gating is enforced in Rust at
the dispatch site and is documented here as a structural assumption rather
than a Lean theorem.
-/

namespace ToolExecution

/-- The 6 persisted states of the tool-call lifecycle. -/
inductive ToolCallState where
  | pending
  | running
  | completed
  | failed
  | timedOut
  | cancelled
  deriving DecidableEq, Repr

namespace ToolCallState

/-- String vocabulary persisted in `AgentToolCall.lifecycle_state`. -/
def toDefraDB : ToolCallState → String
  | .pending => "pending"
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .timedOut => "timedOut"
  | .cancelled => "cancelled"

/-- Parse the persisted vocabulary. -/
def fromDefraDB? : String → Option ToolCallState
  | "pending" => some .pending
  | "running" => some .running
  | "completed" => some .completed
  | "failed" => some .failed
  | "timedOut" => some .timedOut
  | "cancelled" => some .cancelled
  | _ => none

theorem fromDefraDB_toDefraDB (s : ToolCallState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

/-- Exhaustive constructor list for Rust conformance vocabulary generation. -/
def all : List ToolCallState :=
  [ .pending, .running, .completed, .failed, .timedOut, .cancelled ]

theorem all_complete (s : ToolCallState) : s ∈ all := by
  cases s <;> simp [all]

instance : HasTerminal ToolCallState where
  isTerminal s :=
    s = .completed ∨ s = .failed ∨ s = .timedOut ∨ s = .cancelled
  isTerminal_dec s :=
    match s with
    | .completed => isTrue (Or.inl rfl)
    | .failed => isTrue (Or.inr (Or.inl rfl))
    | .timedOut => isTrue (Or.inr (Or.inr (Or.inl rfl)))
    | .cancelled => isTrue (Or.inr (Or.inr (Or.inr rfl)))
    | .pending => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))
    | .running => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))

end ToolCallState

end ToolExecution
