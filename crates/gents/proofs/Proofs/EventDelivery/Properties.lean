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

    This remains a finite-prefix predicate because the conformance boundary
    emits executable finite traces. Short prefixes whose length is below the
    window size are vacuously fair; the live source bindings therefore use
    `rescanBoundedBy = 1` and emit concrete `persist → rescanTick → handle`
    witnesses so D1's rescan window is exercised. The `0` sentinel remains
    unsatisfiable for non-trivial traces and must not be used by live D1
    source bindings. Infinite-stream or tick-indexed latency refinements — the
    universal "every fair schedule delivers within bound" form, as opposed to
    this existential reachability witness — belong in the liveness taxonomy layer
    (#557) rather than this executable trace contract. -/
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
    (`b ≥ 2` → the two-action witness is shorter than the checked window) or
    satisfied by the leading `rescanTick` (`b = 1`). The executable
    conformance rows use the stronger three-action `persist → rescanTick →
    handle` shape with `b = 1`, which prevents live EventSource and
    SubagentSource bindings from closing D1 by the old `0`-sentinel gap. -/
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

/-- **D2 — Fair-delivery latency witness.** When `d` is persistent and not
    yet processed, there exists a two-action subscription-path trace
    `[enqueue d, handle d]` that lands `d` in `handled` without using
    `rescanTick`. Documentation property; not load-bearing.

    The interpretation: under fair subscription delivery (no permanent
    drops), the subscription path closes convergence in two actions
    instead of paying the full rescan-cadence latency. -/
theorem D2_fair_delivery_latency
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      d ∈ w'.handled ∧
      Action.rescanTick ∉ actions := by
  let w₁ : World :=
    { w₀ with subscriptionQueue := d :: w₀.subscriptionQueue }
  let w₂ : World :=
    { w₁ with handled := d :: w₁.handled
            , processedSet := d :: w₁.processedSet
            , subscriptionQueue := w₁.subscriptionQueue.erase d }
  refine ⟨[.enqueue d, .handle d], w₂, ?_, ?_, ?_⟩
  · -- TraceOf w₀ [.enqueue d, .handle d] w₂
    have h_enq : Transition w₀ (.enqueue d) w₁ := Transition.enqueue w₀ d h_persisted
    have h_mem_q : d ∈ w₁.subscriptionQueue := List.mem_cons_self _ _
    have h_handle : Transition w₁ (.handle d) w₂ :=
      Transition.handle w₁ d h_mem_q h_unprocessed
    exact TraceOf.cons h_enq (TraceOf.cons h_handle TraceOf.nil)
  · -- d ∈ w₂.handled
    show d ∈ d :: w₁.handled
    exact List.mem_cons_self _ _
  · -- .rescanTick ∉ [.enqueue d, .handle d]
    intro h
    simp at h

/-- **C1 — Processed-set excludes re-handle.** For any source instance,
    while `d ∈ processedSet`, no `handle d` action is admissible. Watcher-
    relevant: the 30s `processed_request_ids` cooldown enforces this
    operationally; the contract makes it a structural invariant. -/
theorem C1_processed_set_excludes_handle
    (w : World) (d : DocId) (a : Action) (w' : World)
    (h_processed : d ∈ w.processedSet)
    (h : Transition w a w') :
    a ≠ .handle d := by
  intro h_eq
  rw [h_eq] at h
  cases h with
  | handle _ _ h_unprocessed =>
    exact h_unprocessed h_processed

end EventDelivery
