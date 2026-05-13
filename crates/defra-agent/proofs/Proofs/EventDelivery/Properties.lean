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

end EventDelivery
