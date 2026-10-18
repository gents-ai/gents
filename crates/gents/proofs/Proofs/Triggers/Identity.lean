import Mathlib.Data.List.Basic
import Mathlib.Tactic.IntervalCases
import Mathlib.Tactic.Linarith

namespace Triggers.Durable

structure Identity where
  owner : String
  trigger : String
  collection : String
  document : String
  deriving DecidableEq, Repr

def component (value : String) : String := s!"{value.length}:{value}"

def Identity.key (id : Identity) : String :=
  component id.owner ++ component id.trigger ++ component id.collection ++ component id.document

def Identity.requestId (id : Identity) : String := "trigger-request:" ++ id.key

def Identity.sessionId (id : Identity) : String := "trigger-session:" ++ id.key

def Identity.outcomeId (id : Identity) : String := "outcome:" ++ id.key


private def digitValue (c : Char) : Nat := c.toNat - 48

private def decimalValue : List Char → Nat
  | [] => 0
  | c :: cs => digitValue c * 10 ^ cs.length + decimalValue cs

private theorem digit_value (n : Nat) (h : n < 10) :
    digitValue (Nat.digitChar n) = n := by
  interval_cases n <;> decide

private theorem digit_not_colon (n : Nat) (h : n < 10) :
    Nat.digitChar n ≠ ':' := by
  interval_cases n <;> decide

private theorem digits_value (fuel n : Nat) (ds : List Char) (h : n < fuel) :
    decimalValue (Nat.toDigitsCore 10 fuel n ds) =
      n * 10 ^ ds.length + decimalValue ds := by
  induction fuel generalizing n ds with
  | zero => omega
  | succ fuel ih =>
    rw [Nat.toDigitsCore]
    split_ifs with hz
    · simp only [decimalValue, digit_value _ (Nat.mod_lt _ (by omega))]
      have : n % 10 = n := Nat.mod_eq_of_lt (by omega)
      simp [this]
    · rw [ih (n / 10) (Nat.digitChar (n % 10) :: ds) (by omega)]
      simp only [List.length_cons, decimalValue, digit_value _ (Nat.mod_lt _ (by omega)), pow_succ]
      have hn := Nat.mod_add_div n 10
      have hm := congrArg (fun k => k * 10 ^ ds.length) hn
      nlinarith

private theorem digits_no_colon (fuel n : Nat) (ds : List Char)
    (h : ':' ∉ ds) : ':' ∉ Nat.toDigitsCore 10 fuel n ds := by
  induction fuel generalizing n ds with
  | zero => exact h
  | succ fuel ih =>
    simp only [Nat.toDigitsCore]
    split_ifs
    · simp only [List.mem_cons, not_or]
      exact ⟨(digit_not_colon _ (Nat.mod_lt _ (by omega))).symm, h⟩
    · apply ih
      simp only [List.mem_cons, not_or]
      exact ⟨(digit_not_colon _ (Nat.mod_lt _ (by omega))).symm, h⟩

private theorem repr_value (n : Nat) : decimalValue (Nat.toDigits 10 n) = n := by
  simpa [Nat.toDigits, decimalValue] using digits_value (n + 1) n [] (by omega)

private theorem repr_no_colon (n : Nat) : ':' ∉ Nat.toDigits 10 n := by
  exact digits_no_colon (n + 1) n [] (by simp)

private theorem before_colon (xs ys : List Char) (h : ':' ∉ xs) :
    (xs ++ ':' :: ys).takeWhile (· != ':') = xs := by
  rw [List.takeWhile_append_of_pos]
  · simp
  · intro c hc
    simp only [bne_iff_ne]
    intro heq
    subst c
    exact h hc

theorem component_append_injective (a b x y : String)
    (h : component a ++ x = component b ++ y) : a = b ∧ x = y := by
  have hl := congrArg String.data h
  simp only [component, String.data_append] at hl
  simp only [toString, List.nil_append, List.append_nil] at hl
  change (Nat.toDigits 10 a.length ++ [':'] ++ a.data) ++ x.data =
    (Nat.toDigits 10 b.length ++ [':'] ++ b.data) ++ y.data at hl
  simp only [List.append_assoc, List.singleton_append, List.cons_append] at hl
  have hp := congrArg (List.takeWhile (· != ':')) hl
  rw [before_colon _ _ (repr_no_colon _), before_colon _ _ (repr_no_colon _)] at hp
  have hn := congrArg decimalValue hp
  rw [repr_value, repr_value] at hn
  have ht := List.append_cancel_left (hp ▸ hl)
  have hv := List.cons.inj ht
  have he := List.append_inj (s₁ := a.data) (s₂ := b.data) (t₁ := x.data) (t₂ := y.data) hv.2 hn
  exact ⟨String.ext he.1, String.ext he.2⟩

theorem Identity.key_injective : Function.Injective Identity.key := by
  intro a b h
  unfold Identity.key at h
  simp only [String.append_assoc] at h
  obtain ⟨ho, h⟩ := component_append_injective _ _ _ _ h
  obtain ⟨ht, h⟩ := component_append_injective _ _ _ _ h
  obtain ⟨hc, h⟩ := component_append_injective _ _ _ _ h
  have hd : a.document = b.document :=
    (component_append_injective a.document b.document "" "" (by simpa using h)).1
  cases a
  cases b
  simp_all

theorem Identity.requestId_injective : Function.Injective Identity.requestId := by
  intro a b h
  apply Identity.key_injective
  apply String.ext
  have hh := congrArg String.data h
  simp only [Identity.requestId, String.data_append] at hh
  exact List.append_cancel_left hh

theorem Identity.sessionId_injective : Function.Injective Identity.sessionId := by
  intro a b h
  apply Identity.key_injective
  apply String.ext
  have hh := congrArg String.data h
  simp only [Identity.sessionId, String.data_append] at hh
  exact List.append_cancel_left hh

theorem Identity.outcomeId_injective : Function.Injective Identity.outcomeId := by
  intro a b h
  apply Identity.key_injective
  apply String.ext
  have hh := congrArg String.data h
  simp only [Identity.outcomeId, String.data_append] at hh
  exact List.append_cancel_left hh

end Triggers.Durable
