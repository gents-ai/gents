import Proofs.Basic

inductive ManagedExecState where
  | pendingSpawn
  | running
  | exited
  | killSignaled
  | killed
  | spawnFailed
  | reapFailed
  deriving DecidableEq, Repr

namespace ManagedExecState

def toDefraDB : ManagedExecState → String
  | .pendingSpawn => "pendingSpawn"
  | .running => "running"
  | .exited => "exited"
  | .killSignaled => "killSignaled"
  | .killed => "killed"
  | .spawnFailed => "spawnFailed"
  | .reapFailed => "reapFailed"

def fromDefraDB? : String → Option ManagedExecState
  | "pendingSpawn" => some .pendingSpawn
  | "running" => some .running
  | "exited" => some .exited
  | "killSignaled" => some .killSignaled
  | "killed" => some .killed
  | "spawnFailed" => some .spawnFailed
  | "reapFailed" => some .reapFailed
  | _ => none

theorem fromDefraDB_toDefraDB (s : ManagedExecState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

def all : List ManagedExecState :=
  [ .pendingSpawn
  , .running
  , .exited
  , .killSignaled
  , .killed
  , .spawnFailed
  , .reapFailed
  ]

theorem all_complete (s : ManagedExecState) : s ∈ all := by
  cases s <;> simp [all]

instance : HasTerminal ManagedExecState where
  isTerminal s :=
    s = .exited ∨ s = .killed ∨ s = .spawnFailed ∨ s = .reapFailed
  isTerminal_dec s :=
    match s with
    | .exited => isTrue (Or.inl rfl)
    | .killed => isTrue (Or.inr (Or.inl rfl))
    | .spawnFailed => isTrue (Or.inr (Or.inr (Or.inl rfl)))
    | .reapFailed => isTrue (Or.inr (Or.inr (Or.inr rfl)))
    | .pendingSpawn => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))
    | .running => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))
    | .killSignaled => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))

end ManagedExecState

structure ManagedExecContext where
  state : ManagedExecState
  deadline : Time
  now : Time
  killSignaledAt : Option Time := none
  exitCode : Option Int := none
  deriving Repr

namespace ManagedExecContext

def deadlineExceeded (c : ManagedExecContext) : Prop :=
  c.deadline < c.now

instance (c : ManagedExecContext) : Decidable c.deadlineExceeded :=
  Nat.decLt c.deadline c.now

end ManagedExecContext
