import Proofs.Basic

inductive ExecutionOrigin where
  | interactive
  | scheduled
  deriving DecidableEq, Repr

namespace ExecutionOrigin

def toDefraDB : ExecutionOrigin → String
  | .interactive => "interactive"
  | .scheduled => "scheduled"

def fromDefraDB? : String → Option ExecutionOrigin
  | "interactive" => some .interactive
  | "scheduled" => some .scheduled
  | _ => none

theorem fromDefraDB_toDefraDB (origin : ExecutionOrigin) :
    fromDefraDB? origin.toDefraDB = some origin := by
  cases origin <;> rfl

end ExecutionOrigin

structure BackendId where
  val : String
  deriving DecidableEq, Repr

structure BackendState where
  max_concurrent : Nat
  available : Bool
  deriving DecidableEq, Repr

inductive AdmissionState where
  | released
  | waiting
  | acquired
  | executing
  deriving DecidableEq, Repr

namespace AdmissionState

def holdsSlot : AdmissionState → Prop
  | .acquired => True
  | .executing => True
  | .released => False
  | .waiting => False

instance : DecidablePred holdsSlot := by
  intro s
  cases s with
  | released => exact isFalse (by simp [holdsSlot])
  | waiting => exact isFalse (by simp [holdsSlot])
  | acquired => exact isTrue (by simp [holdsSlot])
  | executing => exact isTrue (by simp [holdsSlot])

end AdmissionState

export AdmissionState (holdsSlot)

structure SchedulerState where
  running : BackendId → Nat
  backends : BackendId → BackendState

namespace SchedulerState

@[ext] theorem ext
    {s t : SchedulerState}
    (h_running : s.running = t.running)
    (h_backends : s.backends = t.backends) :
    s = t := by
  cases s
  cases t
  cases h_running
  cases h_backends
  rfl

def capacityInvariant (s : SchedulerState) : Prop :=
  ∀ bid : BackendId, s.running bid ≤ (s.backends bid).max_concurrent

end SchedulerState
