import Proofs.Automation.EventDelivery.Executable
import Lean

namespace EventDelivery

inductive TraceActionKind where
  | persist | rescan | handle | deliver | update | notify | drop
  deriving Lean.ToJson

/-- What wakes a source: a change notification from the database's event
transport, or the periodic rescan tick the host runs. -/
inductive Wake where
  | notification | tick
  deriving DecidableEq, Repr

/-- A notification is only a wake. On every wake the source reads its pending
work from the durable arrival cursor, never from the notification, so a
notification the transport drops, delays or redelivers changes when work is
offered, not which work; the tick is the wake no transport can lose.
External premise: the transport may drop, delay or redeliver notifications. -/
def Wake.action : Wake → Action
  | .notification | .tick => .rescanTick

theorem notification_wake_is_a_rescan : Wake.notification.action = Wake.tick.action := rfl

structure TraceAction where
  kind : TraceActionKind
  document : String
  deriving Lean.ToJson

/-- An edit of a source document after its arrival is never a second arrival:
the source's dedupe is monotone-once over its lifetime, so an update takes no
delivery action. -/
def TraceAction.actions (a : TraceAction) : List Action :=
  match a.kind with
  | .persist => [.persist ⟨a.document⟩]
  | .rescan => [Wake.tick.action]
  | .notify => [Wake.notification.action]
  | .drop => []
  | .handle => [.handle ⟨a.document⟩]
  | .deliver => [.deliverFromQueue ⟨a.document⟩]
  | .update => []

structure ConvergenceInput where
  name : String
  initialWorld : World
  actions : List TraceAction
  deriving Lean.ToJson

def convergenceTrace (name document : String) : ConvergenceInput :=
  ⟨name, World.empty,
   [⟨.persist, document⟩, ⟨.rescan, ""⟩, ⟨.handle, document⟩, ⟨.rescan, ""⟩]⟩

/-- An offered event can be lost before its durable acknowledgement. The cursor
still names unprocessed work, so the next source instance must offer it again. -/
def unacknowledgedTrace (document : String) : ConvergenceInput :=
  ⟨"event_source_unacknowledged_redelivery", World.empty,
   [⟨.persist, document⟩, ⟨.rescan, ""⟩, ⟨.deliver, document⟩,
    ⟨.rescan, ""⟩, ⟨.handle, document⟩, ⟨.rescan, ""⟩]⟩

/-- A document that existed when its source was configured is seeded as
processed: the cursor starts after it. -/
def seededWorld (documents : List String) : World :=
  { World.empty with persistentSet := documents.map DocId.mk
                     processedSet := documents.map DocId.mk }

def seededUpdateTrace (document : String) : ConvergenceInput :=
  ⟨"event_source_seeded_document_update", seededWorld [document],
   [⟨.update, document⟩, ⟨.rescan, ""⟩]⟩

def firstSeenUpdateTrace (document : String) : ConvergenceInput :=
  ⟨"event_source_first_seen_then_update", World.empty,
   [⟨.persist, document⟩, ⟨.rescan, ""⟩, ⟨.handle, document⟩,
    ⟨.update, document⟩, ⟨.rescan, ""⟩]⟩

/-- The document's notification is lost; the next rescan offers it. -/
def droppedNotificationTrace (document : String) : ConvergenceInput :=
  ⟨"event_source_dropped_notification_rescan", World.empty,
   [⟨.persist, document⟩, ⟨.drop, document⟩, ⟨.rescan, ""⟩, ⟨.handle, document⟩,
    ⟨.rescan, ""⟩]⟩

/-- The transport delivers the document's notification twice; the second wake
finds nothing pending. -/
def redeliveredNotificationTrace (document : String) : ConvergenceInput :=
  ⟨"event_source_redelivered_notification", World.empty,
   [⟨.persist, document⟩, ⟨.notify, document⟩, ⟨.handle, document⟩,
    ⟨.notify, document⟩, ⟨.rescan, ""⟩]⟩

/-- The notification arrives after a rescan already delivered the document. -/
def lateNotificationTrace (document : String) : ConvergenceInput :=
  ⟨"event_source_late_notification", World.empty,
   [⟨.persist, document⟩, ⟨.rescan, ""⟩, ⟨.handle, document⟩, ⟨.notify, document⟩]⟩

def finalWorld (i : ConvergenceInput) : Option World :=
  replay? i.initialWorld (i.actions.flatMap TraceAction.actions)

def offeredAlong : World → List Action → Option (List DocId)
  | _, [] => some []
  | w, a :: rest => do
    let next ← step? w a
    let later ← offeredAlong next rest
    let offered := if a.isRescan then
      next.subscriptionQueue.take (next.subscriptionQueue.length - w.subscriptionQueue.length)
      else []
    pure (offered ++ later)

structure ConvergenceObservation where
  offered : List String
  handled : List String
  offeredAfter : List String
  status : String
  deriving Lean.ToJson

def convergenceObservation (i : ConvergenceInput) : Option ConvergenceObservation := do
  let w ← finalWorld i
  let again ← step? w .rescanTick
  let offered ← offeredAlong i.initialWorld (i.actions.flatMap TraceAction.actions)
  pure ⟨offered.map (·.raw), w.handled.map (·.raw), again.subscriptionQueue.map (·.raw),
    if again = w then "fixed_point" else "pending"⟩

theorem emitted_trace_reaches_fixed_point (name document : String) :
    let i := convergenceTrace name document
    ∃ w, finalWorld i = some w ∧ step? w .rescanTick = some w ∧
      w.handled.Nodup ∧ (⟨document⟩ : DocId) ∈ w.handled := by
  simp [convergenceTrace, finalWorld, TraceAction.actions, Wake.action, replay?, step?, World.empty]

theorem unacknowledged_delivery_can_be_offered_again (document : String) :
    ∃ w, replay? World.empty [.persist ⟨document⟩, .rescanTick,
      .deliverFromQueue ⟨document⟩, .rescanTick] = some w ∧
      (⟨document⟩ : DocId) ∈ w.subscriptionQueue ∧
      (⟨document⟩ : DocId) ∉ w.processedSet ∧ w.handled = [] := by
  simp [replay?, step?, World.empty]

theorem unacknowledged_delivery_is_offered_twice (document : String) :
    offeredAlong World.empty ((unacknowledgedTrace document).actions.flatMap TraceAction.actions) =
      some [⟨document⟩, ⟨document⟩] := by
  simp [offeredAlong, unacknowledgedTrace, TraceAction.actions, Wake.action, Action.isRescan, step?, World.empty]

theorem unacknowledged_delivery_converges (document : String) :
    ∃ w, finalWorld (unacknowledgedTrace document) = some w ∧
      step? w .rescanTick = some w ∧ w.handled = [⟨document⟩] := by
  simp [unacknowledgedTrace, finalWorld, TraceAction.actions, Wake.action, replay?, step?, World.empty]

theorem emitted_trace_is_fair (name document : String) (inst : SourceInstance)
    (h : inst.rescanBoundedBy = 1) :
    Fair inst ((convergenceTrace name document).actions.flatMap TraceAction.actions) := by
  have hl : (convergenceTrace name document).actions.flatMap TraceAction.actions =
      [.persist ⟨document⟩, .rescanTick, .handle ⟨document⟩, .rescanTick] := by
    simp [convergenceTrace, TraceAction.actions, Wake.action]
  rw [hl]
  intro i hi
  simp [h] at hi
  by_cases h0 : i = 0
  · subst i; exact ⟨1, by decide, by simp [h], rfl⟩
  by_cases h1 : i = 1
  · subst i; exact ⟨1, by decide, by simp [h], rfl⟩
  have h2 : i = 2 := by omega
  subst i
  exact ⟨3, by decide, by simp [h], rfl⟩

theorem event_source_skips_event_for_doc_already_seen_at_subscribe (document : String) :
    convergenceObservation (seededUpdateTrace document) =
      some ⟨[], [], [], "fixed_point"⟩ := by
  simp [convergenceObservation, seededUpdateTrace, finalWorld, offeredAlong, seededWorld,
    TraceAction.actions, Wake.action, Action.isRescan, replay?, step?, World.empty]

theorem event_source_fires_for_first_seen_doc_then_skips_updates (document : String) :
    convergenceObservation (firstSeenUpdateTrace document) =
      some ⟨[document], [document], [], "fixed_point"⟩ := by
  simp [convergenceObservation, firstSeenUpdateTrace, finalWorld, offeredAlong,
    TraceAction.actions, Wake.action, Action.isRescan, replay?, step?, World.empty]

theorem rescan_offers_every_unprocessed (w : World) (d : DocId)
    (persisted : d ∈ w.persistentSet) (unprocessed : d ∉ w.processedSet) :
    ∃ w', step? w .rescanTick = some w' ∧ d ∈ w'.subscriptionQueue := by
  refine ⟨_, rfl, ?_⟩
  simp [persisted, unprocessed]

theorem trace_actions_never_release (actions : List TraceAction) (d : DocId) :
    Action.release d ∉ actions.flatMap TraceAction.actions := by
  simp only [List.mem_flatMap, not_exists, not_and]
  intro a _
  cases h : a.kind <;> simp [TraceAction.actions, Wake.action, h]

/-- Whatever the transport does to notifications (drop, delay, redelivery), a
trace from an empty world delivers each document at most once. -/
theorem lossy_transport_delivers_at_most_once (actions : List TraceAction) (w : World)
    (h : replay? World.empty (actions.flatMap TraceAction.actions) = some w) :
    w.handled.Nodup :=
  epoch_delivery_at_most_once (by simp [EpochInvariant, World.empty])
    (fun d => trace_actions_never_release actions d) h

theorem dropped_notification_is_delivered_once_by_rescan (document : String) :
    convergenceObservation (droppedNotificationTrace document) =
      some ⟨[document], [document], [], "fixed_point"⟩ := by
  simp [convergenceObservation, droppedNotificationTrace, finalWorld, offeredAlong,
    TraceAction.actions, Wake.action, Action.isRescan, replay?, step?, World.empty]

theorem redelivered_notification_delivers_once (document : String) :
    convergenceObservation (redeliveredNotificationTrace document) =
      some ⟨[document], [document], [], "fixed_point"⟩ := by
  simp [convergenceObservation, redeliveredNotificationTrace, finalWorld, offeredAlong,
    TraceAction.actions, Wake.action, Action.isRescan, replay?, step?, World.empty]

theorem late_notification_delivers_once (document : String) :
    convergenceObservation (lateNotificationTrace document) =
      some ⟨[document], [document], [], "fixed_point"⟩ := by
  simp [convergenceObservation, lateNotificationTrace, finalWorld, offeredAlong,
    TraceAction.actions, Wake.action, Action.isRescan, replay?, step?, World.empty]

end EventDelivery

