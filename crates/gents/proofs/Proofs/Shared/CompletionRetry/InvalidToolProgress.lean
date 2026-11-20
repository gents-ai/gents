import Proofs.Shared.CompletionRetry.State

namespace CompletionRetry.InvalidToolProgress

/-- Request-execution-local accounting in the existing owned completion loop.
`recordDurable` is called only after the outcome has been persisted/threaded.
It does not terminalize a request: exhaustion asks the existing stream error
path and execution lease owner to do so. Assumes serialized dispatch; a stream
containing several calls must check eligibility before each dispatch. The
request-wide budget limits resources spent on invalid, denied, and unknown tool
calls even when successful calls occur between them. The allowance is
`InferenceExecution.max_invalid_tool_calls`; this is its value when unset. -/
def defaultLimit : Nat := 8

/-- The limit an execution runs with, from its configured field: unset takes
`default`, a positive value is used as is, and a non-positive one is refused
(`none`), so validation rejects it before any execution starts. Zero is refused
rather than read as "no guard": an allowance of zero is exhausted before the
first call (`zero_allowance_exhausted`), and a repeated-failure limit of zero
admits no bounded state (`RepeatedToolFailure.zero_failure_limit_unbounded`). -/
def configuredLimit (default : Nat) : Option Int → Option Nat
  | none => some default
  | some n => if 0 < n then some n.toNat else none

theorem configuredLimit_positive (default : Nat) (configured : Option Int) (n : Nat)
    (hd : 0 < default) (h : configuredLimit default configured = some n) : 0 < n := by
  cases configured with
  | none => simp [configuredLimit] at h; omega
  | some c =>
      simp only [configuredLimit] at h
      split at h
      · simp at h; omega
      · simp at h

theorem configuredLimit_refuses_zero (default : Nat) :
    configuredLimit default (some 0) = none := rfl

inductive Outcome
  | invalidArguments | policyDenied | unknownTool
  | success | ordinaryFailure | skipped | backgroundCompletion
  deriving DecidableEq, Repr

def invalid : Outcome → Bool
  | .invalidArguments | .policyDenied | .unknownTool => true
  | _ => false

/-- `limit` is the execution's resolved allowance, fixed for the execution. -/
structure State where
  invalidUsed : Nat := 0
  limit : Nat := defaultLimit
  deriving DecidableEq, Repr

def canDispatch (s : State) : Bool := s.invalidUsed < s.limit

def recordDurable (s : State) (outcome : Outcome) : State :=
  if canDispatch s && invalid outcome then { s with invalidUsed := s.invalidUsed + 1 } else s

def exhausted (s : State) : Bool := !canDispatch s

def run (s : State) : List Outcome → State
  | [] => s
  | outcome :: tail => run (recordDurable s outcome) tail

theorem bounded_step (s : State) (o : Outcome) (h : s.invalidUsed ≤ s.limit) :
    (recordDurable s o).invalidUsed ≤ s.limit := by
  unfold recordDurable canDispatch
  split <;> simp_all <;> omega

theorem limit_fixed (s : State) (o : Outcome) : (recordDurable s o).limit = s.limit := by
  unfold recordDurable
  split <;> rfl

theorem bounded_trace (s : State) (outcomes : List Outcome) (h : s.invalidUsed ≤ s.limit) :
    (run s outcomes).invalidUsed ≤ s.limit := by
  induction outcomes generalizing s with
  | nil => exact h
  | cons o tail ih =>
      have := ih _ (by rw [limit_fixed]; exact bounded_step s o h)
      rwa [limit_fixed] at this

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

theorem invalid_rank_decreases (s : State) (o : Outcome)
    (h : canDispatch s = true) (hi : invalid o = true) :
    s.limit - (recordDurable s o).invalidUsed < s.limit - s.invalidUsed := by
  rw [invalid_spends_one s o h hi]
  simp [canDispatch] at h
  omega

theorem last_invalid_stops (s : State) (o : Outcome) (h : invalid o = true)
    (hl : s.invalidUsed + 1 = s.limit) :
    exhausted (recordDurable s o) = true := by
  have hc : canDispatch s = true := by simp [canDispatch]; omega
  simp [exhausted, canDispatch, invalid_spends_one s o hc h, limit_fixed]
  omega

theorem zero_allowance_exhausted : exhausted { limit := 0 } = true := rfl

theorem eighth_invalid_stops (o : Outcome) (h : invalid o = true) :
    exhausted (recordDurable { invalidUsed := 7 } o) = true :=
  last_invalid_stops _ o h rfl

theorem eight_invalids_exhaust :
    run {} (List.replicate 8 .invalidArguments) = { invalidUsed := 8 } := by decide

end CompletionRetry.InvalidToolProgress
