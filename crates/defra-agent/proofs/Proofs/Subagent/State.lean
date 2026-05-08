import Proofs.Basic

/-!
# Subagent State

Mode and policy enums attached to `ToolCallContext` to support multi-flight,
foreground/background scheduling, and detachable subagent invocations.

`BridgedState` (a paired parent-child `ComposedState`) is added in a later task
once `ComposedState` has been refactored to multi-flight.
-/

namespace Subagent

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

end Subagent
