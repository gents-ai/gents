import Proofs.Basic

namespace Subagent

inductive BackgroundedKind where
  | Subagent
  | Tool
  deriving DecidableEq, Repr

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

inductive AwaitMode where
  | foreground
  | background
  deriving DecidableEq, Repr

namespace AwaitMode

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

inductive CancelPolicy where
  | cascade
  | detach
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

def maxSubagentDepth : Nat := 3

def maxBackgroundedPerParent : Nat := 8

end Subagent
