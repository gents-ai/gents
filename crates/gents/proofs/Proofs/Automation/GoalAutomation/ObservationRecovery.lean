import Proofs.Background.Observation
import Proofs.GoalAutomation.Observation

namespace GoalAutomation.ObservationRecovery

/-- A durable rescan refreshes the signed Goal projection even while the
notification feed remains open. Notifications are hints, not evidence that
the previously projected Goal is current. Terminating canonical reads and
eventual polling of the periodic timer are external scheduling premises. -/
def observe (schedule : Background.Observation.State) (input : Background.Observation.Input)
    (actor : String) (scope : Observation.Scope) (unique : Bool)
    (canonical : Option Observation.GoalReference) (previous : Option Observation.Observation) :
    Option Observation.Observation :=
  match Background.Observation.wake schedule input with
  | .rescan | .changed => (Observation.project? actor scope unique canonical).orElse fun _ => previous
  | _ => previous

theorem lost_notification_rescan_uses_canonical_goal
    (schedule : Background.Observation.State) (input : Background.Observation.Input)
    (actor : String) (scope : Observation.Scope) (unique : Bool)
    (canonical : Option Observation.GoalReference) (previous : Option Observation.Observation)
    (projected : Observation.Observation)
    (live : input.cancelled = false) (before : input.now < input.deadlineAt)
    (due : schedule.rescanAt ≤ input.now)
    (valid : Observation.project? actor scope unique canonical = some projected) :
    observe schedule input actor scope unique canonical previous = some projected := by
  simp only [observe, Background.Observation.due_rescan_cannot_be_starved schedule input live before due,
    valid]
  rfl

theorem cancellation_keeps_previous_projection
    (schedule : Background.Observation.State) (input : Background.Observation.Input)
    (actor : String) (scope : Observation.Scope) (unique : Bool)
    (canonical : Option Observation.GoalReference) (previous : Option Observation.Observation)
    (cancelled : input.cancelled = true) :
    observe schedule input actor scope unique canonical previous = previous := by
  simp only [observe, Background.Observation.cancellation_precedes_all_wakes schedule input cancelled]

end GoalAutomation.ObservationRecovery
