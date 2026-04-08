import Proofs.Basic

/-!
# Layer 4: Scheduling Vocabulary

Shared types for backend binding and scheduler admission.
-/

/-- The origin of a unit of work. -/
inductive ExecutionOrigin where
  | interactive
  | scheduled
  deriving DecidableEq, Repr

/-- Backend identifier. Opaque — we only need equality. -/
structure BackendId where
  val : String
  deriving DecidableEq, Repr

/-- Backend state as observed by the scheduler. -/
structure BackendState where
  max_concurrent : Nat
  available : Bool
  deriving DecidableEq, Repr

/-- Admission state with respect to scheduler capacity. -/
inductive AdmissionState where
  | released
  | waiting
  | acquired
  | executing
  deriving DecidableEq, Repr

namespace AdmissionState

/-- Whether the scheduler currently considers this work to hold a slot. -/
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

/-- Aggregate scheduler state over all backends visible to a daemon. -/
structure SchedulerState where
  running : BackendId → Nat
  backends : BackendId → BackendState

namespace SchedulerState

/-- Extensionality for scheduler states. -/
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

/-- Capacity safety: no backend is tracked as overloaded. -/
def capacityInvariant (s : SchedulerState) : Prop :=
  ∀ bid : BackendId, s.running bid ≤ (s.backends bid).max_concurrent

end SchedulerState
