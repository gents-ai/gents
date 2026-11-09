import Proofs.Basic

namespace RequestBackendWake

/-- A fallback is bounded and deterministic across restart; health observations
may wake sooner, but never extend the original execution deadline. -/
def delayMs (attempt : Nat) : Nat := min 30000 (1000 * 2 ^ min attempt 5)

def fallback (now deadline attempt : Nat) : Option Nat :=
  if now < deadline then some (min deadline (now + delayMs attempt)) else none

def freshHealthy (waitStarted now : Nat) (observed : Option Nat) : Bool :=
  match observed with
  | none => false
  | some time => decide (waitStarted ≤ time ∧ time ≤ now)

theorem stale_health_cannot_wake (waitStarted now observed : Nat)
    (h : observed < waitStarted) : freshHealthy waitStarted now (some observed) = false := by
  simp [freshHealthy, Nat.not_le.mpr h]

theorem fallback_does_not_extend_deadline (now deadline attempt wake : Nat)
    (h : fallback now deadline attempt = some wake) : wake ≤ deadline := by
  unfold fallback at h
  split at h
  · cases h
    exact Nat.min_le_left _ _
  · simp at h

theorem fallback_not_before_observation (now deadline attempt wake : Nat)
    (h : fallback now deadline attempt = some wake) : now ≤ wake := by
  unfold fallback at h
  split at h
  · rename_i hnow
    cases h
    exact Nat.le_min.mpr ⟨by omega, by omega⟩
  · simp at h

theorem expired_has_no_wake (now deadline attempt : Nat) (h : deadline ≤ now) :
    fallback now deadline attempt = none := by
  simp [fallback, Nat.not_lt.mpr h]

end RequestBackendWake
