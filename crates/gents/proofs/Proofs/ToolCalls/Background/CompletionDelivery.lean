namespace Background.CompletionDelivery

inductive TerminalizationResult where
  | won
  | lost
  deriving DecidableEq

structure NotificationState where
  terminal : Bool
  notificationPresent : Bool
  deliveryMarked : Bool
  deriving DecidableEq, Repr

def publishOnce (state : NotificationState) : NotificationState :=
  { state with notificationPresent := true, deliveryMarked := true }

/-- The terminal CAS and notification append are distinct durable writes;
publication state is recoverable even when publication fails. -/
def terminalize
    (result : TerminalizationResult)
    (state : NotificationState) : NotificationState :=
  match result with
  | .won => { state with terminal := true }
  | .lost => state

def publishCandidate
    (result : TerminalizationResult)
    (state : NotificationState) : NotificationState :=
  match result with
  | .won => publishOnce (terminalize .won state)
  | .lost => state

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

end Background.CompletionDelivery
