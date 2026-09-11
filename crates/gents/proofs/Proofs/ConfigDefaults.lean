import Proofs.Basic

/-! Signed configuration limits are validated before conversion to natural numbers.
Defaults and minimum values remain with the capability that owns each setting. -/
namespace ConfigDefaults

/-- A minimum of one is a positive timer/capacity; zero permits an empty queue. -/
def resolveNat (fallback minimum : Nat) : Option Int → Option Nat
  | none => some fallback
  | some value => if (minimum : Int) ≤ value then some value.toNat else none

theorem resolveNat_lower_bound (fallback minimum : Nat) (authored : Option Int)
    (value : Nat) (hd : minimum ≤ fallback)
    (h : resolveNat fallback minimum authored = some value) : minimum ≤ value := by
  cases authored with
  | none => simp [resolveNat] at h; omega
  | some n =>
    simp only [resolveNat] at h
    split at h
    · simp only [Option.some.injEq] at h; omega
    · simp at h

/-- Positive controls use exactly the same decoder as zero-permitting limits. -/
theorem resolveNat_positive (fallback : Nat) (authored : Option Int) :
    resolveNat fallback 1 authored =
      match authored with
      | none => some fallback
      | some n => if 0 < n then some n.toNat else none := by
  cases authored with
  | none => rfl
  | some n =>
    simp only [resolveNat]
    congr 1

/-- Resolve a default/maximum pair once for every capability. If the documented
maximum is absent, it follows the resolved default (bash execution semantics). -/
def resolveBounded (fallback : Nat) (maximumFallback : Option Nat)
    (authored maximum : Option Int) : Option (Nat × Nat) := do
  let value ← resolveNat fallback 1 authored
  let cap ← resolveNat (maximumFallback.getD value) 1 maximum
  if value ≤ cap then some (value, cap) else none

theorem resolved_maximum_covers_default (fallback : Nat) (maximumFallback : Option Nat)
    (authored maximum : Option Int) (pair : Nat × Nat)
    (h : resolveBounded fallback maximumFallback authored maximum = some pair) :
    pair.1 ≤ pair.2 := by
  simp only [resolveBounded, bind, Option.bind] at h
  split at h <;> simp_all
  split at h <;> simp_all
  rcases h with ⟨bound, rfl⟩
  exact bound

theorem absent_maximum_follows (fallback : Nat) (authored : Option Int) (pair : Nat × Nat)
    (h : resolveBounded fallback none authored none = some pair) : pair.2 = pair.1 := by
  simp only [resolveBounded, resolveNat, bind, Option.bind, Option.getD_none] at h
  split at h <;> simp_all
  cases h
  rfl

end ConfigDefaults
