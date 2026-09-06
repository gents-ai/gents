import Proofs.CompletionRetry.State

/-! Request-execution-local accounting in the existing owned completion loop.
`recordDurable` is called only after the outcome has been persisted/threaded.
It does not terminalize a request: exhaustion asks the existing stream error path
and execution lease owner to do so. The abstraction assumes serialized dispatch;
a stream containing several calls must check eligibility before each dispatch.
-/
namespace CompletionRetry.InvalidToolProgress

def limit : Nat := 8

inductive Outcome
  | invalidArguments | policyDenied | unknownTool
  | success | ordinaryFailure | skipped | backgroundCompletion
  deriving DecidableEq, Repr

def invalid : Outcome → Bool
  | .invalidArguments | .policyDenied | .unknownTool => true
  | _ => false

structure State where
  invalidUsed : Nat := 0
  deriving DecidableEq, Repr

def canDispatch (s : State) : Bool := s.invalidUsed < limit

def recordDurable (s : State) (outcome : Outcome) : State :=
  if canDispatch s && invalid outcome then ⟨s.invalidUsed + 1⟩ else s

def exhausted (s : State) : Bool := !canDispatch s

def run (s : State) : List Outcome → State
  | [] => s
  | outcome :: tail => run (recordDurable s outcome) tail

theorem bounded_step (s : State) (o : Outcome) (h : s.invalidUsed ≤ limit) :
    (recordDurable s o).invalidUsed ≤ limit := by
  unfold recordDurable canDispatch
  split <;> simp_all <;> omega

theorem bounded_trace (s : State) (outcomes : List Outcome) (h : s.invalidUsed ≤ limit) :
    (run s outcomes).invalidUsed ≤ limit := by
  induction outcomes generalizing s with
  | nil => exact h
  | cons o tail ih => exact ih _ (bounded_step s o h)

theorem no_reset (s : State) (o : Outcome) :
    s.invalidUsed ≤ (recordDurable s o).invalidUsed := by
  unfold recordDurable
  split <;> simp_all

theorem uncharged_stutters (s : State) (o : Outcome) (h : invalid o = false) :
    recordDurable s o = s := by simp [recordDurable, h]

theorem terminal_absorbing (s : State) (o : Outcome) (h : exhausted s = true) :
    recordDurable s o = s := by simp_all [exhausted, recordDurable]

theorem invalid_spends_one (s : State) (o : Outcome)
    (h : canDispatch s = true) (hi : invalid o = true) :
    (recordDurable s o).invalidUsed = s.invalidUsed + 1 := by
  simp [recordDurable, h, hi]

/-- A strict natural-valued rank decreases on every accepted invalid outcome.
Valid outcomes cannot replenish it. This bounds invalid churn even when valid
calls are interspersed; it does not claim termination of infinite valid work. -/
theorem invalid_rank_decreases (s : State) (o : Outcome)
    (h : canDispatch s = true) (hi : invalid o = true) :
    limit - (recordDurable s o).invalidUsed < limit - s.invalidUsed := by
  rw [invalid_spends_one s o h hi]
  simp [canDispatch] at h
  omega

theorem eighth_invalid_stops (o : Outcome) (h : invalid o = true) :
    exhausted (recordDurable ⟨7⟩ o) = true := by
  simp [recordDurable, canDispatch, limit, h, exhausted]

theorem eight_invalids_exhaust :
    run ⟨0⟩ (List.replicate 8 .invalidArguments) = ⟨8⟩ := by decide

end CompletionRetry.InvalidToolProgress
