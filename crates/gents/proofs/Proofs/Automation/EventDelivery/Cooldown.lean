import Lean

namespace EventDelivery.Cooldown

/-- The local watcher expires a mark at 30 seconds and clears a live cohort
only when it exceeds 10,000 entries. Expiry and eviction end the dedupe epoch. -/
def lifetimeMillis : Nat := 30000

def capacity : Nat := 10000

structure Cohort where
  count : Nat
  ageMillis : Nat
  deriving Lean.ToJson

def retained (c : Cohort) : Nat :=
  let live := if c.ageMillis < lifetimeMillis then c.count else 0
  if capacity < live then 0 else live

theorem expiry_ends_epoch (count age : Nat) (h : lifetimeMillis ≤ age) :
    retained ⟨count, age⟩ = 0 := by
  simp [retained, Nat.not_lt.mpr h, capacity]

theorem capacity_ends_epoch (count age : Nat) (h : capacity < count) :
    retained ⟨count, age⟩ = 0 := by
  by_cases active : age < lifetimeMillis <;> simp [retained, active, h]

theorem live_cohort_is_retained (count age : Nat)
    (h_age : age < lifetimeMillis) (h_count : count ≤ capacity) :
    retained ⟨count, age⟩ = count := by
  simp [retained, h_age, Nat.not_lt.mpr h_count]

end EventDelivery.Cooldown
