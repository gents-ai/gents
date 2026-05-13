import Proofs.EventDelivery.Contract

namespace EventDelivery

/-- Companion termination measure: number of persistent docs not yet in
    `processedSet`. Strictly decreases under `handle`; bounded-non-increasing
    under every other action. -/
def pendingWork (w : World) : Nat :=
  (w.persistentSet.filter (fun d => d ∉ w.processedSet)).length

/-- A list of actions is `Fair` for an instance when every window of
    `inst.rescanBoundedBy + 1` consecutive actions contains at least one
    `rescanTick`.

    When `inst.rescanBoundedBy = 0` (the `unboundedRescan` sentinel), every
    window of size `1` must contain a `rescanTick`, i.e. EVERY action must
    be `rescanTick`. Since real action lists also contain `persist`, etc.,
    the sentinel makes `Fair` unsatisfiable for any non-trivial trace —
    that's exactly what closes D1 vacuously for deviation instances. -/
def Fair (inst : SourceInstance) (actions : List Action) : Prop :=
  ∀ i : Nat, i + inst.rescanBoundedBy < actions.length →
    ∃ j : Nat, i ≤ j ∧ j ≤ i + inst.rescanBoundedBy ∧
      (actions[j]?).map Action.isRescan = some true

/-- The empty action list is trivially fair. -/
theorem Fair.nil (inst : SourceInstance) : Fair inst [] := by
  intro i h_lt
  simp at h_lt

/-- A single `rescanTick` is fair for any instance with `rescanBoundedBy > 0`. -/
theorem Fair.singleton_rescanTick
    (inst : SourceInstance) (h_pos : 0 < inst.rescanBoundedBy) :
    Fair inst [.rescanTick] := by
  intro i h_lt
  simp only [List.length_singleton] at h_lt
  have h_i : i = 0 := by omega
  have h_b : inst.rescanBoundedBy = 0 := by omega
  exact absurd h_b (Nat.ne_zero_of_lt h_pos)

/-- A finite trace witness: a sequence of actions chaining transitions
    from a start world to an end world. Reflexive-transitive closure of
    `Transition` indexed by the action list. -/
inductive TraceOf : World → List Action → World → Prop where
  | nil  {w : World} : TraceOf w [] w
  | cons {w₁ a w₂ as w₃} :
      Transition w₁ a w₂ → TraceOf w₂ as w₃ → TraceOf w₁ (a :: as) w₃

/-- **D1 — Delivery convergence.** When the instance has a positive rescan
    bound and the doc is persistent and not yet processed, there exists a
    fair action sequence that drives the doc to `handled`.

    Witness: `[rescanTick, handle d]`. The rescan dumps every unprocessed
    persistent doc into the subscription queue, after which `handle d` is
    enabled and lands `d` in `handled`. Fairness holds because
    `rescanBoundedBy ≥ 1` makes the window check either trivially closed
    (`b ≥ 2` → vacuous) or satisfied by the leading `rescanTick` (`b = 1`).

    For instances using `SourceInstance.unboundedRescan = 0`, this theorem is
    vacuous (`Fair` becomes unsatisfiable for any 2-action list); that is
    exactly the deviation that the Rust gap-fill in EventSource /
    SubagentSource closes operationally. -/
theorem D1_delivery_convergence
    (inst : SourceInstance)
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet)
    (h_inst_pos : 0 < inst.rescanBoundedBy) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair inst actions ∧
      d ∈ w'.handled := by
  -- Intermediate worlds.
  let w₁ : World :=
    { w₀ with subscriptionQueue :=
        (w₀.persistentSet.filter (fun x => x ∉ w₀.processedSet)) ++ w₀.subscriptionQueue }
  let w₂ : World :=
    { w₁ with handled := d :: w₁.handled
            , processedSet := d :: w₁.processedSet
            , subscriptionQueue := w₁.subscriptionQueue.erase d }
  refine ⟨[.rescanTick, .handle d], w₂, ?_, ?_, ?_⟩
  · -- TraceOf w₀ [.rescanTick, .handle d] w₂
    have h_rescan : Transition w₀ .rescanTick w₁ := Transition.rescanTick w₀
    -- d ∈ w₁.subscriptionQueue: by construction d is in the filtered prefix.
    have h_mem_q : d ∈ w₁.subscriptionQueue := by
      show d ∈ (w₀.persistentSet.filter (fun x => x ∉ w₀.processedSet)) ++ w₀.subscriptionQueue
      apply List.mem_append.mpr
      left
      apply List.mem_filter.mpr
      refine ⟨h_persisted, ?_⟩
      simp [h_unprocessed]
    have h_unproc₁ : d ∉ w₁.processedSet := h_unprocessed
    have h_handle : Transition w₁ (.handle d) w₂ := Transition.handle w₁ d h_mem_q h_unproc₁
    exact TraceOf.cons h_rescan (TraceOf.cons h_handle TraceOf.nil)
  · -- Fair inst [.rescanTick, .handle d]
    intro i h_window
    -- actions.length = 2; i + rescanBoundedBy < 2 with rescanBoundedBy ≥ 1 forces i = 0 and b = 1.
    have h_len : ([Action.rescanTick, Action.handle d]).length = 2 := rfl
    rw [h_len] at h_window
    have h_i : i = 0 := by omega
    have h_b : inst.rescanBoundedBy = 1 := by omega
    subst h_i
    refine ⟨0, Nat.le_refl 0, ?_, ?_⟩
    · -- 0 ≤ 0 + rescanBoundedBy
      omega
    · -- ([.rescanTick, .handle d])[0]?.map isRescan = some true
      rfl
  · -- d ∈ w₂.handled
    show d ∈ d :: w₁.handled
    exact List.mem_cons_self _ _

end EventDelivery
