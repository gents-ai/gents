import Proofs.Automation.EventDelivery.Properties

namespace EventDelivery

def step? (w : World) : Action → Option World
  | .persist d => if d ∉ w.persistentSet then
      some { w with persistentSet := d :: w.persistentSet } else none
  | .depersist d => if d ∈ w.persistentSet then
      some { w with persistentSet := w.persistentSet.erase d } else none
  | .enqueue d => if d ∈ w.persistentSet then
      some { w with subscriptionQueue := d :: w.subscriptionQueue } else none
  | .drop d | .deliverFromQueue d => if d ∈ w.subscriptionQueue then
      some { w with subscriptionQueue := w.subscriptionQueue.erase d } else none
  | .rescanTick => some { w with subscriptionQueue :=
      (w.persistentSet.filter (fun d => d ∉ w.processedSet)) ++ w.subscriptionQueue }
  | .handle d => if d ∈ w.subscriptionQueue ∧ d ∉ w.processedSet then
      some { w with handled := d :: w.handled
                    processedSet := d :: w.processedSet
                    subscriptionQueue := w.subscriptionQueue.erase d } else none
  | .release d => if d ∈ w.processedSet then
      some { w with processedSet := w.processedSet.filter (fun x => x ≠ d) } else none

theorem step_sound {w w' : World} {a : Action} (h : step? w a = some w') :
    Transition w a w' := by
  cases a with
  | persist d =>
    simp [step?] at h
    rcases h with ⟨h, rfl⟩
    exact Transition.persist _ _ h
  | depersist d =>
    simp [step?] at h
    rcases h with ⟨h, rfl⟩
    exact Transition.depersist _ _ h
  | enqueue d =>
    simp [step?] at h
    rcases h with ⟨h, rfl⟩
    exact Transition.enqueue _ _ h
  | drop d =>
    simp [step?] at h
    rcases h with ⟨h, rfl⟩
    exact Transition.drop _ _ h
  | deliverFromQueue d =>
    simp [step?] at h
    rcases h with ⟨h, rfl⟩
    exact Transition.deliverFromQueue _ _ h
  | rescanTick =>
    simp [step?] at h
    subst w'
    simpa only [decide_not] using (Transition.rescanTick w)
  | handle d =>
    simp [step?] at h
    rcases h with ⟨⟨queued, unprocessed⟩, rfl⟩
    exact Transition.handle _ _ queued unprocessed
  | release d =>
    simp [step?] at h
    rcases h with ⟨h, rfl⟩
    simpa only [decide_not] using (Transition.release w d h)

theorem step_complete {w w' : World} {a : Action} (h : Transition w a w') :
    step? w a = some w' := by
  cases h <;> simp_all [step?]

def replay? : World → List Action → Option World
  | w, [] => some w
  | w, a :: rest => (step? w a).bind (fun next => replay? next rest)

theorem replay_sound {w w' : World} {actions : List Action}
    (h : replay? w actions = some w') : TraceOf w actions w' := by
  induction actions generalizing w with
  | nil => simp [replay?] at h; subst w'; exact TraceOf.nil
  | cons a rest ih =>
    cases hs : step? w a with
    | none => simp [replay?, hs] at h
    | some next =>
      simp [replay?, hs] at h
      exact TraceOf.cons (step_sound hs) (ih h)

theorem replay_complete {w w' : World} {actions : List Action}
    (h : TraceOf w actions w') : replay? w actions = some w' := by
  induction h with
  | nil => rfl
  | cons h _ ih => simp [replay?, step_complete h, ih]

theorem processed_handle_rejected (w : World) (d : DocId)
    (h : d ∈ w.processedSet) : step? w (.handle d) = none := by
  simp [step?, h]

theorem quiescent_rescan_fixed (w : World)
    (h : ∀ d ∈ w.persistentSet, d ∈ w.processedSet) :
    step? w .rescanTick = some w := by
  have empty : w.persistentSet.filter (fun d => d ∉ w.processedSet) = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro d hd
    simp [h d hd]
  cases w
  simp_all [step?]

end EventDelivery

namespace EventDelivery

def EpochInvariant (w : World) : Prop :=
  w.handled.Nodup ∧ ∀ d ∈ w.handled, d ∈ w.processedSet

theorem epoch_step_preserves {w w' : World} {a : Action}
    (hi : EpochInvariant w) (ha : ∀ d, a ≠ .release d)
    (ht : Transition w a w') : EpochInvariant w' := by
  cases ht with
  | persist | depersist | enqueue | drop | deliverFromQueue | rescanTick => exact hi
  | handle d queued fresh =>
    constructor
    · exact List.nodup_cons.mpr ⟨fun hd => fresh (hi.2 d hd), hi.1⟩
    · intro x hx
      rcases List.mem_cons.mp hx with rfl | hx
      · exact List.mem_cons_self _ _
      · exact List.mem_cons_of_mem _ (hi.2 x hx)
  | release d marked => exact False.elim (ha d rfl)

theorem trace_preserves_epoch {w w' : World} {actions : List Action}
    (ht : TraceOf w actions w') :
    EpochInvariant w → (∀ d, Action.release d ∉ actions) → EpochInvariant w' := by
  induction ht with
  | nil => intro hi _; exact hi
  | @cons w a next rest final ht _ ih =>
    intro hi ha
    apply ih (epoch_step_preserves hi (fun d eq => ha d (List.mem_cons.mpr (Or.inl eq.symm))) ht)
    intro d hd
    exact ha d (List.mem_cons_of_mem _ hd)

theorem epoch_delivery_at_most_once {w w' : World} {actions : List Action}
    (hi : EpochInvariant w) (ha : ∀ d, Action.release d ∉ actions)
    (hr : replay? w actions = some w') : w'.handled.Nodup :=
  (trace_preserves_epoch (replay_sound hr) hi ha).1

def deliveredWorld (d : DocId) : World :=
  (replay? World.empty [.persist d, .rescanTick, .handle d]).getD World.empty

theorem single_document_converges (d : DocId) :
    replay? World.empty [.persist d, .rescanTick, .handle d] = some (deliveredWorld d) ∧
    d ∈ (deliveredWorld d).handled ∧
    step? (deliveredWorld d) .rescanTick = some (deliveredWorld d) ∧
    step? (deliveredWorld d) (.handle d) = none := by
  simp [deliveredWorld, replay?, step?, World.empty]

theorem release_permits_redelivery (d : DocId) :
    ((replay? (deliveredWorld d) [.release d, .rescanTick, .handle d]).getD World.empty).handled =
      [d, d] := by
  simp [deliveredWorld, replay?, step?, World.empty]

end EventDelivery
