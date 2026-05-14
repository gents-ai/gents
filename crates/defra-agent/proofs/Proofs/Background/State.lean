import Proofs.Basic

/-!
# Subagent State

Mode and policy enums attached to `ToolCallContext` to support multi-flight,
foreground/background scheduling, and detachable subagent invocations.

`BridgedState` (a paired parent-child `ComposedState`) is added in a later task
once `ComposedState` has been refactored to multi-flight.
-/

namespace Subagent

/-- The kind of backgrounded work represented by a bridge row. R4 only had
    Subagent rows; R6 admits ordinary tool executions as a second kind. -/
inductive BackgroundedKind where
  | Subagent
  | Tool
  deriving DecidableEq, Repr

/-- Terminal vocabulary observed by the bridge projector. `.running` is the
    non-terminal catch-all used before the second leg reaches a terminal. -/
inductive ChildTerminal where
  | running
  | completed
  | failed
  | dead
  | interrupted
  | superseded
  deriving DecidableEq, Repr

namespace ChildTerminal

def isFailure : ChildTerminal → Prop
  | .failed => True
  | .dead => True
  | .interrupted => True
  | .superseded => True
  | .running => False
  | .completed => False

instance (t : ChildTerminal) : Decidable t.isFailure := by
  cases t <;> simp [isFailure] <;> infer_instance

end ChildTerminal

/-- Whether the parent's narrative is blocked on this tool's terminal state. -/
inductive AwaitMode where
  | foreground   -- parent.advance / begin_inference are blocked while this tool is non-terminal
  | background   -- parent advances independently; tool runs concurrently
  deriving DecidableEq, Repr

namespace AwaitMode

/-- Persisted vocabulary in `AgentToolCall.await_mode`. -/
def toDefraDB : AwaitMode → String
  | .foreground => "foreground"
  | .background => "background"

/-- Parse the persisted vocabulary. -/
def fromDefraDB? : String → Option AwaitMode
  | "foreground" => some .foreground
  | "background" => some .background
  | _ => none

theorem fromDefraDB_toDefraDB (m : AwaitMode) :
    fromDefraDB? m.toDefraDB = some m := by
  cases m <;> rfl

def all : List AwaitMode := [ .foreground, .background ]

theorem all_complete (m : AwaitMode) : m ∈ all := by
  cases m <;> simp [all]

end AwaitMode

/-- Cancel-cascade policy: whether parent termination drives the linked child
    to .interrupted, or detaches the child to its own deadline. -/
inductive CancelPolicy where
  | cascade   -- default; parent terminal ⇒ child.interruptRequestedAt set
  | detach    -- child outlives parent
  deriving DecidableEq, Repr

namespace CancelPolicy

def toDefraDB : CancelPolicy → String
  | .cascade => "cascade"
  | .detach  => "detach"

/-- Parse the persisted vocabulary. -/
def fromDefraDB? : String → Option CancelPolicy
  | "cascade" => some .cascade
  | "detach"  => some .detach
  | _ => none

theorem fromDefraDB_toDefraDB (p : CancelPolicy) :
    fromDefraDB? p.toDefraDB = some p := by
  cases p <;> rfl

def all : List CancelPolicy := [ .cascade, .detach ]

theorem all_complete (p : CancelPolicy) : p ∈ all := by
  cases p <;> simp [all]

end CancelPolicy

/-- Configured cap on subagent recursion depth. Treated as a global parameter
    referenced by the depth-bound theorem; the runtime supplies the concrete
    value from behavior config. -/
def maxSubagentDepth : Nat := 3

/-- R6 ceiling on concurrent non-terminal backgrounded tool rows owned by one
    parent request. -/
def maxBackgroundedPerParent : Nat := 8

end Subagent
