import Batteries.Tactic.OpenPrivate
import Proofs.PluginJob.Types
import Proofs.Triggers.Identity

open private decimalValue repr_no_colon from Proofs.Triggers.Identity

namespace PluginJob

open Triggers.Durable

def SpawnKey.suffix (k : SpawnKey) : String :=
  component k.root ++ component k.slot ++ component (toString k.attempt)

def SpawnKey.retryKey (k : SpawnKey) : String := "job:" ++ k.suffix

def SpawnKey.messageKey (k : SpawnKey) (n : Nat) : String :=
  k.retryKey ++ component "m" ++ component (toString n)

def SpawnKey.sessionId (k : SpawnKey) : String := "job-session:" ++ k.suffix

/-- Reads one length-prefixed component off the front of `cs`. -/
def takeComponent (cs : List Char) : String × List Char :=
  let ds := cs.takeWhile (· != ':')
  let rest := cs.drop (ds.length + 1)
  let n := decimalValue ds
  (String.mk (rest.take n), rest.drop n)

/-- Inverse of `SpawnKey.retryKey`, used to show the encoding is total and
unambiguous. Its Rust counterpart is
`gents_protocol::job::keys::decode_job_retry_key`. -/
def decodeRetryKey? (s : String) : Option SpawnKey :=
  match s.data with
  | 'j' :: 'o' :: 'b' :: ':' :: cs =>
    let (root, r1) := takeComponent cs
    let (slot, r2) := takeComponent r1
    let (attempt, r3) := takeComponent r2
    if r3 = [] then some ⟨root, slot, decimalValue attempt.data⟩ else none
  | _ => none

private theorem takeComponent_component (v : String) (rest : List Char) :
    takeComponent ((component v).data ++ rest) = (v, rest) := by
  have hc : (component v).data = Nat.toDigits 10 v.length ++ ':' :: v.data := by
    simp only [component, String.data_append]
    simp only [toString, List.nil_append, List.append_nil]
    change (Nat.toDigits 10 v.length ++ [':'] ++ v.data) = _
    simp
  have hd : ((Nat.toDigits 10 v.length ++ ':' :: v.data) ++ rest).takeWhile (· != ':')
      = Nat.toDigits 10 v.length := by
    rw [List.append_assoc, List.cons_append]
    exact before_colon _ _ (repr_no_colon _)
  have h1 : ((Nat.toDigits 10 v.length ++ ':' :: v.data) ++ rest).drop
      ((Nat.toDigits 10 v.length).length + 1) = v.data ++ rest := by
    have : (Nat.toDigits 10 v.length ++ ':' :: v.data) ++ rest =
        (Nat.toDigits 10 v.length ++ [':']) ++ (v.data ++ rest) := by simp
    rw [this]
    exact List.drop_left' (by simp)
  have ht : (v.data ++ rest).take v.length = v.data := List.take_left' rfl
  have hr : (v.data ++ rest).drop v.length = rest := List.drop_left' rfl
  simp only [takeComponent, hc, hd, h1, repr_value, ht, hr]

private theorem toString_nat_injective (a b : Nat) (h : toString a = toString b) : a = b := by
  have := congrArg (fun s : String => decimalValue s.data) h
  change decimalValue (Nat.toDigits 10 a) = decimalValue (Nat.toDigits 10 b) at this
  rwa [repr_value, repr_value] at this

private theorem suffix_append_injective (a b : SpawnKey) (x y : String)
    (h : a.suffix ++ x = b.suffix ++ y) : a = b ∧ x = y := by
  unfold SpawnKey.suffix at h
  simp only [String.append_assoc] at h
  obtain ⟨hr, h⟩ := component_append_injective _ _ _ _ h
  obtain ⟨hs, h⟩ := component_append_injective _ _ _ _ h
  obtain ⟨ha, hx⟩ := component_append_injective _ _ _ _ h
  have ha := toString_nat_injective _ _ ha
  cases a
  cases b
  simp_all

private theorem prefix_cancel (p x y : String) (h : p ++ x = p ++ y) : x = y :=
  String.ext (List.append_cancel_left (by simpa only [String.data_append] using congrArg String.data h))

theorem retryKey_injective : Function.Injective SpawnKey.retryKey := by
  intro a b h
  have h := prefix_cancel _ _ _ h
  exact (suffix_append_injective a b "" "" (by simpa using h)).1

theorem sessionId_injective : Function.Injective SpawnKey.sessionId := by
  intro a b h
  have h := prefix_cancel _ _ _ h
  exact (suffix_append_injective a b "" "" (by simpa using h)).1

theorem messageKey_injective (a b : SpawnKey) (n m : Nat)
    (h : a.messageKey n = b.messageKey m) : a = b ∧ n = m := by
  unfold SpawnKey.messageKey SpawnKey.retryKey at h
  simp only [String.append_assoc] at h
  obtain ⟨hab, h⟩ := suffix_append_injective _ _ _ _ (prefix_cancel _ _ _ h)
  have h := prefix_cancel _ _ _ h
  obtain ⟨hn, -⟩ := component_append_injective _ _ "" "" (by simpa using h)
  exact ⟨hab, toString_nat_injective _ _ hn⟩

theorem messageKey_ne_retryKey (a b : SpawnKey) (n : Nat) :
    a.messageKey n ≠ b.retryKey := by
  intro h
  unfold SpawnKey.messageKey SpawnKey.retryKey at h
  simp only [String.append_assoc] at h
  have h := prefix_cancel _ _ _ h
  have h := (suffix_append_injective _ _ _ "" (by simpa using h)).2
  have := congrArg String.length h
  simp only [String.length_append, String.length_empty] at this
  have hm : (component "m").length = 3 := by decide
  omega

theorem decode_retryKey (k : SpawnKey) : decodeRetryKey? k.retryKey = some k := by
  have hk : k.retryKey.data = 'j' :: 'o' :: 'b' :: ':' ::
      ((component k.root).data ++ ((component k.slot).data ++
        ((component (toString k.attempt)).data ++ []))) := by
    simp [SpawnKey.retryKey, SpawnKey.suffix, String.data_append]
  have ha : decimalValue (toString k.attempt).data = k.attempt := repr_value _
  unfold decodeRetryKey?
  rw [hk]
  simp only [takeComponent_component, ha]
  simp

end PluginJob
