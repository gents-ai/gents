/-!
# Background completion publication

The terminal CAS winner publishes a durable notification; recovery repairs a
missing delivery marker. BackgroundCompletion composes notification persistence
with the existing session wake owner, including its canonical Goal exclusion.
-/

namespace Subagent.CompletionDelivery

/-!
The tool/subagent bridge is terminalized with a compare-and-set. A concurrent
recovery or cancellation may win that CAS first. The losing executor must not
publish the outcome it observed locally, because that outcome can contradict
the durable winner. Publication itself is keyed and idempotent so retrying the
winning delivery cannot duplicate the notification.
-/

inductive TerminalizationResult where
  | won
  | lost
  deriving DecidableEq

structure NotificationState where
  terminal : Bool
  notificationPresent : Bool
  deliveryMarked : Bool
  deriving DecidableEq

/-- A stable notification key makes publication an insert-once operation. The
    delivery marker is written only after the notification is durable. -/
def publishOnce (state : NotificationState) : NotificationState :=
  { state with notificationPresent := true, deliveryMarked := true }

/-- The terminal compare-and-set and notification append are distinct durable
    writes. This state is recoverable even when publication fails. -/
def terminalize
    (result : TerminalizationResult)
    (state : NotificationState) : NotificationState :=
  match result with
  | .won => { state with terminal := true }
  | .lost => state

/--
Only a caller that won the durable terminal compare-and-set may publish its
candidate outcome. A loser leaves notification state untouched; a reconciler
may separately project the already-durable winner.
-/
def publishCandidate
    (result : TerminalizationResult)
    (state : NotificationState) : NotificationState :=
  match result with
  | .won => publishOnce (terminalize .won state)
  | .lost => state

/-- A periodic/startup reconciler repairs a terminal row whose durable
    delivery marker is absent. Stable-key publication makes this safe to
    repeat after "append succeeded, marker write failed". -/
def reconcileDelivery (state : NotificationState) : NotificationState :=
  if state.terminal && !state.deliveryMarked then publishOnce state else state

def DeliveryInvariant (state : NotificationState) : Prop :=
  state.deliveryMarked = true → state.notificationPresent = true

theorem losing_terminalizer_does_not_publish
    (state : NotificationState) :
    publishCandidate .lost state = state := by
  rfl

theorem publish_once_idempotent
    (state : NotificationState) :
    publishOnce (publishOnce state) = publishOnce state := by
  cases state
  rfl

theorem winning_publication_is_idempotent
    (state : NotificationState) :
    publishCandidate .won (publishCandidate .won state) =
      publishCandidate .won state := by
  cases state
  rfl

theorem terminal_without_notification_is_repairable
    (state : NotificationState)
    (h_terminal : state.terminal = true)
    (h_sound : DeliveryInvariant state) :
    (reconcileDelivery state).notificationPresent = true := by
  cases state with
  | mk terminal present marked =>
      cases terminal <;> cases present <;> cases marked <;>
        simp_all [reconcileDelivery, publishOnce, DeliveryInvariant]

theorem reconciled_marker_implies_notification
    (state : NotificationState)
    (h_sound : DeliveryInvariant state) :
    (reconcileDelivery state).deliveryMarked = true →
      (reconcileDelivery state).notificationPresent = true := by
  cases state with
  | mk terminal present marked =>
      cases terminal <;> cases present <;> cases marked <;>
        simp_all [reconcileDelivery, publishOnce, DeliveryInvariant]

end Subagent.CompletionDelivery
