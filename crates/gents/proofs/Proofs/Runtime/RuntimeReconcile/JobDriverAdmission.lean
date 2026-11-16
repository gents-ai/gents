import Proofs.Runtime.RuntimeReconcile.ServiceReplacement

/-!
# Job-driver work under runtime retirement

The job driver holds one admitted execution of the runtime's service owner for
each row it has in flight: a recovery hold, a running step, and a step that
waits for a step permit. The execution finishes when the row leaves flight, so
retirement is refused while a hold or a deferred step remains, and a retired
runtime admits no further hold.
-/

namespace RuntimeReconcile.JobDriverAdmission

open RuntimeReconcile.ServiceReplacement

/-- One row as the driver sees it: whether a hold or step has it in flight,
whether a step waits for a permit, whether every step permit is taken, and how
many steps of it have run. -/
structure Driver where
  service : State
  inFlight : Bool
  waiting : Bool
  permitsTaken : Bool
  stepsRun : Nat
  deriving DecidableEq, Repr

inductive Op where
  /-- A recovery hold of the row; `retireDuring` asks for retirement inside it
  and `wake` wakes the row inside it. -/
  | hold (wake retireDuring : Bool)
  | releasePermits
  | retire
  deriving DecidableEq, Repr

structure Observation where
  admitted : Bool
  retiredDuring : Option Bool
  stepping : Bool
  stepsRun : Nat
  deriving DecidableEq, Repr

/-- The operator's authenticated retirement of the observed build. -/
def operatorRetire (s : State) : Option State :=
  step s (.retire ⟨true, true⟩)

/-- The admitted execution of a row that leaves flight finishes. -/
def leaveFlight (d : Driver) : Driver :=
  { d with inFlight := false, waiting := false, service := (step d.service .finish).getD d.service }

/-- A woken row's step runs now when a permit is free and then leaves flight;
without a permit it waits in flight, keeping its admitted execution. -/
def runOrWait (d : Driver) : Driver :=
  if d.permitsTaken then { d with inFlight := true, waiting := true }
  else leaveFlight { d with stepsRun := d.stepsRun + 1 }

def observe (d : Driver) (admitted : Bool) (retiredDuring : Option Bool) : Observation :=
  { admitted, retiredDuring, stepping := d.inFlight, stepsRun := d.stepsRun }

def apply (d : Driver) : Op → Driver × Observation
  | .hold wake retireDuring =>
    if d.inFlight then (d, observe d false none) else
    match step d.service .admit with
    | none => (d, observe d false none)
    | some admitted =>
      let during := if retireDuring then some (operatorRetire admitted).isSome else none
      let held := { d with service := ((if retireDuring then operatorRetire admitted else none).getD admitted), inFlight := true }
      let after := if wake then runOrWait held else leaveFlight held
      (after, observe after true during)
  | .releasePermits =>
    let released := { d with permitsTaken := false }
    let after := if d.waiting then runOrWait released else released
    (after, observe after true none)
  | .retire =>
    match operatorRetire d.service with
    | some s => ({ d with service := s }, observe d true none)
    | none => (d, observe d false none)

def run : Driver → List Op → List Observation
  | _, [] => []
  | d, op :: rest => (apply d op).2 :: run (apply d op).1 rest

def idle (permitsTaken : Bool) : Driver :=
  { service := ⟨.accepting, 0, 0⟩, inFlight := false, waiting := false, permitsTaken, stepsRun := 0 }

theorem recovery_hold_and_deferred_step_keep_runtime_retirement_busy :
    run (idle true) [.hold true true, .retire, .releasePermits, .retire, .hold false false] =
      [ ⟨true, some false, true, 0⟩, ⟨false, none, true, 0⟩, ⟨true, none, false, 1⟩
      , ⟨true, none, false, 1⟩, ⟨false, none, false, 1⟩ ] := by
  decide

end RuntimeReconcile.JobDriverAdmission
