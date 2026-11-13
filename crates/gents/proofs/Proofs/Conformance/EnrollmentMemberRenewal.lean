import Proofs.P2P.Enrollment.AutoRenewal
import Proofs.Conformance.Family

namespace Conformance.EnrollmentMemberRenewal

open Lean Enrollment.AutoRenewal

deriving instance Lean.ToJson for WakeAction, WakeStep, WakeTrace, WakeObservation

def instants : List Nat := [0, 500, 1000, 60000]

def wakeSteps : List WakeStep :=
  [WakeAction.due, WakeAction.wake].flatMap fun action =>
    instants.map fun atMillis => { action, atMillis }

def sequences : Nat → List (List WakeStep)
  | 0 => [[]]
  | n + 1 => wakeSteps.flatMap fun step => (sequences n).map (step :: ·)

def traces : List WakeTrace :=
  (sequences 3).map (fun steps => ({ steps := steps } : WakeTrace)) ++ [{ steps := offerScenario }]

@[conformance_family]
def clock : Family WakeTrace (List WakeObservation) where
  inputs := traces
  model := fun trace => runClock {} trace.steps

end Conformance.EnrollmentMemberRenewal
