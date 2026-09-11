import Proofs.Background.State

namespace Subagent

/-- Existing background admission gate. A successful create consumes one slot.
The caller supplies the count of live background rows observed by its owner;
this contract does not assert atomicity between concurrent creators. -/
def admitBackground (liveCount : Nat) : Option Nat :=
  if liveCount < maxBackgroundedPerParent then some (liveCount + 1) else none

theorem admitted_background_count_bounded (before after : Nat)
    (h : admitBackground before = some after) :
    after = before + 1 ∧ after ≤ maxBackgroundedPerParent := by
  unfold admitBackground at h
  split at h
  · cases h
    exact ⟨rfl, by omega⟩
  · contradiction

theorem full_background_budget_rejected (liveCount : Nat)
    (h : maxBackgroundedPerParent ≤ liveCount) : admitBackground liveCount = none := by
  simp [admitBackground, Nat.not_lt.mpr h]

end Subagent
