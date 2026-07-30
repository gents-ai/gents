import Proofs.Basic
import Proofs.Persistence
import Proofs.ToolExecution.Policy
import Proofs.Background.State

namespace ToolExecution

inductive ToolCallState where
  | pending
  | awaitingApproval
  | running
  | completed
  | failed
  | timedOut
  | cancelled
  deriving DecidableEq, Repr

namespace ToolCallState

def toDefraDB : ToolCallState → String
  | .pending => "pending"
  | .awaitingApproval => "awaitingApproval"
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .timedOut => "timedOut"
  | .cancelled => "cancelled"

def fromDefraDB? : String → Option ToolCallState
  | "pending" => some .pending
  | "awaitingApproval" => some .awaitingApproval
  | "running" => some .running
  | "completed" => some .completed
  | "failed" => some .failed
  | "timedOut" => some .timedOut
  | "cancelled" => some .cancelled
  | _ => none

theorem fromDefraDB_toDefraDB (s : ToolCallState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

def all : List ToolCallState :=
  [ .pending, .awaitingApproval, .running, .completed, .failed, .timedOut, .cancelled ]

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
    | .awaitingApproval =>
        isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))
    | .running => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))

end ToolCallState

end ToolExecution

namespace ToolExecution

abbrev ToolCallId := Nat

inductive ApprovalDecision where
  | approved
  | denied
  deriving DecidableEq, Repr

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
  approval       : Option ApprovalDecision := none
  awaitMode      : Subagent.AwaitMode := .foreground
  cancelPolicy   : Subagent.CancelPolicy := .cascade
  childRequestId : Option RequestId := none
  deriving Repr

namespace ToolCallContext

def deadlineExceeded (c : ToolCallContext) : Prop :=
  c.currentTime > c.deadline

instance (c : ToolCallContext) : Decidable c.deadlineExceeded :=
  Nat.decLt c.deadline c.currentTime

def cancellable (c : ToolCallContext) : Prop :=
  c.state = .pending ∨ c.state = .awaitingApproval ∨ c.state = .running

instance (c : ToolCallContext) : Decidable c.cancellable := by
  unfold cancellable; infer_instance

def linkedTo (c : ToolCallContext) (rid : RequestId) : Prop :=
  c.requestId = rid

instance (c : ToolCallContext) (rid : RequestId) : Decidable (c.linkedTo rid) := by
  unfold linkedTo; infer_instance

end ToolCallContext

end ToolExecution
