import Proofs.Shared.Basic

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

/-- A resumed request continues its turn budget rather than restarting it: its
next turn follows every provider turn it durably completed before parking, so
repeated outages cannot buy turns beyond the configured maximum. -/
def resumedTurn : List Nat → Nat
  | [] => 0
  | turn :: rest => max (turn + 1) (resumedTurn rest)

theorem completed_turn_precedes_resumed (completed : List Nat) (turn : Nat)
    (h : turn ∈ completed) : turn < resumedTurn completed := by
  induction completed with
  | nil => simp at h
  | cons first rest ih =>
      simp only [List.mem_cons] at h
      simp only [resumedTurn]
      cases h with
      | inl heq => subst turn; omega
      | inr hrest =>
          have bound := ih hrest
          omega

theorem unstarted_request_resumes_at_first_turn : resumedTurn [] = 0 := rfl

/-- A resumed execution continues its request's provider call sequence: the
counter starts at the largest `call_seq` the request durably recorded before
it parked, so the next call is numbered after every earlier attempt. The call
sequence is then the request's attempt history order across executions; a
restart at zero made a resumed call sort among the parked execution's
attempts. Executions of one request never overlap (park and wake are
generation-fenced in `RequestExecutionLease`) and every call row is created
before its provider dispatch, so the read at wake sees every earlier call. -/
def resumedCallSeq : List Nat → Nat
  | [] => 0
  | seq :: rest => max seq (resumedCallSeq rest)

theorem persisted_call_precedes_resumed (persisted : List Nat) (seq : Nat)
    (h : seq ∈ persisted) : seq ≤ resumedCallSeq persisted := by
  induction persisted with
  | nil => simp at h
  | cons first rest ih =>
      simp only [List.mem_cons] at h
      simp only [resumedCallSeq]
      cases h with
      | inl heq => subst seq; omega
      | inr hrest =>
          have bound := ih hrest
          omega

theorem unstarted_request_numbers_from_one : resumedCallSeq [] + 1 = 1 := rfl

end RequestBackendWake
