import Proofs.Automation.EventDelivery.Executable
import Lean

namespace EventDelivery

inductive TraceActionKind where
  | persist | rescan | handle | deliver | update
  deriving Lean.ToJson

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
  | .rescan => [.rescanTick]
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
  simp [convergenceTrace, finalWorld, TraceAction.actions, replay?, step?, World.empty]

theorem unacknowledged_delivery_can_be_offered_again (document : String) :
    ∃ w, replay? World.empty [.persist ⟨document⟩, .rescanTick,
      .deliverFromQueue ⟨document⟩, .rescanTick] = some w ∧
      (⟨document⟩ : DocId) ∈ w.subscriptionQueue ∧
      (⟨document⟩ : DocId) ∉ w.processedSet ∧ w.handled = [] := by
  simp [replay?, step?, World.empty]

theorem unacknowledged_delivery_is_offered_twice (document : String) :
    offeredAlong World.empty ((unacknowledgedTrace document).actions.flatMap TraceAction.actions) =
      some [⟨document⟩, ⟨document⟩] := by
  simp [offeredAlong, unacknowledgedTrace, TraceAction.actions, Action.isRescan, step?, World.empty]

theorem unacknowledged_delivery_converges (document : String) :
    ∃ w, finalWorld (unacknowledgedTrace document) = some w ∧
      step? w .rescanTick = some w ∧ w.handled = [⟨document⟩] := by
  simp [unacknowledgedTrace, finalWorld, TraceAction.actions, replay?, step?, World.empty]

theorem emitted_trace_is_fair (name document : String) (inst : SourceInstance)
    (h : inst.rescanBoundedBy = 1) :
    Fair inst ((convergenceTrace name document).actions.flatMap TraceAction.actions) := by
  have hl : (convergenceTrace name document).actions.flatMap TraceAction.actions =
      [.persist ⟨document⟩, .rescanTick, .handle ⟨document⟩, .rescanTick] := by
    simp [convergenceTrace, TraceAction.actions]
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
    TraceAction.actions, Action.isRescan, replay?, step?, World.empty]

theorem event_source_fires_for_first_seen_doc_then_skips_updates (document : String) :
    convergenceObservation (firstSeenUpdateTrace document) =
      some ⟨[document], [document], [], "fixed_point"⟩ := by
  simp [convergenceObservation, firstSeenUpdateTrace, finalWorld, offeredAlong,
    TraceAction.actions, Action.isRescan, replay?, step?, World.empty]

end EventDelivery

