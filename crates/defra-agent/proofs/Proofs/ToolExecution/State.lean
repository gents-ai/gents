import Proofs.Basic
import Proofs.Persistence
import Proofs.ToolExecution.Policy
import Proofs.Background.State

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

namespace ToolExecution

/-- Identifier for an individual tool-call row. -/
abbrev ToolCallId := Nat

/-- Mutable per-tool-call context that transitions carry along. -/
structure ToolCallContext where
  callId         : ToolCallId
  requestId      : RequestId
  state          : ToolCallState
  operation      : ToolOperation
  deadline       : Time
  startedAt      : Option Time := none
  currentTime    : Time
  failureClass   : Option FailureClass := none
  persistence    : PersistenceState
  -- Subagent extensions:
  awaitMode      : Subagent.AwaitMode := .foreground
  cancelPolicy   : Subagent.CancelPolicy := .cascade
  childRequestId : Option RequestId := none
  deriving Repr

namespace ToolCallContext

/-- Whether the tool's deadline has been exceeded. -/
def deadlineExceeded (c : ToolCallContext) : Prop :=
  c.currentTime > c.deadline

instance (c : ToolCallContext) : Decidable c.deadlineExceeded :=
  Nat.decLt c.deadline c.currentTime

/-- A call is cancellable iff it is in a non-terminal pre-state. -/
def cancellable (c : ToolCallContext) : Prop :=
  c.state = .pending ∨ c.state = .running

instance (c : ToolCallContext) : Decidable c.cancellable := by
  unfold cancellable; infer_instance

/-- Linkage to a parent request. -/
def linkedTo (c : ToolCallContext) (rid : RequestId) : Prop :=
  c.requestId = rid

instance (c : ToolCallContext) (rid : RequestId) : Decidable (c.linkedTo rid) := by
  unfold linkedTo; infer_instance

end ToolCallContext

end ToolExecution
