import Proofs.Shared.CompletionRetry.Properties

/-! The provider-limit verdict a failed call's terminal row records (#3923).
The admission permit writes a call's terminal before its error reaches the
owned loop, so it is the one writer of the verdict. It decides under the same
rate-limit wait cap the loop's retry policy holds (`Budget.maxWait`), carried
to it on the admitted model, so a recorded usage limit is exactly a failure the
loop does not retry (`recorded_usage_limit_is_the_loops_permanent_failure`).
Readers of a failed call read this verdict, never its failure text. -/
namespace InferenceCall.ProviderLimit

open CompletionRetry

/-- What a usage refusal says about the limit it hit, each part only when the
provider states it: the window the limit counts over, in minutes, the tokens
counted in that window and the limit on them. -/
structure Statement where
  windowMinutes : Option Nat := none
  tokensUsed : Option Nat := none
  tokensLimit : Option Nat := none
  deriving DecidableEq, Repr

/-- A provider limit one attempt reported, before any retry policy reads it:
the provider's usage exhausted (resetting when it reports a reset, and saying
what it states of the limit), or a throttle (retryable at the wake it reports,
when it reports one). -/
inductive Report where
  | usageExhausted (resetsAt : Option Time) (statement : Statement)
  | throttled (wake : Option Time)
  deriving DecidableEq, Repr

inductive Verdict where
  | throttled
  | usageLimited
  deriving DecidableEq, Repr

namespace Verdict

def toDefraDB : Verdict → String
  | .throttled => "throttled"
  | .usageLimited => "usage_limited"

def fromDefraDB? : String → Option Verdict
  | "throttled" => some .throttled
  | "usage_limited" => some .usageLimited
  | _ => none

theorem fromDefraDB_toDefraDB (verdict : Verdict) :
    fromDefraDB? verdict.toDefraDB = some verdict := by
  cases verdict <;> rfl

end Verdict

/-- The verdict a failed call's row records with its failure reason. Only a
usage limit carries a reset time and a statement; InferenceCall knows nothing
of who reads it. -/
structure Recorded where
  verdict : Verdict
  resetsAt : Option Time
  statement : Statement := {}
  deriving DecidableEq, Repr

/-- The report under a rate-limit wait cap: a throttle whose wake lies beyond
the cap is the provider's usage limit, resetting at that wake. A throttle
states nothing about a usage limit, so such a limit records an empty
statement. -/
def verdict (now : Time) (maxWait : Option Time) : Report → Recorded
  | .usageExhausted resetsAt statement => ⟨.usageLimited, resetsAt, statement⟩
  | .throttled none => ⟨.throttled, none, {}⟩
  | .throttled (some wake) =>
      if withinMaxWait now wake maxWait then ⟨.throttled, none, {}⟩
      else ⟨.usageLimited, some wake, {}⟩

theorem a_usage_exhaustion_keeps_its_reset_and_statement (now : Time) (maxWait : Option Time)
    (resetsAt : Option Time) (statement : Statement) :
    verdict now maxWait (.usageExhausted resetsAt statement) =
      ⟨.usageLimited, resetsAt, statement⟩ := rfl

/-- Only a usage refusal states its limit: whatever a throttle's wake, its
row carries an empty statement. -/
theorem a_throttle_states_no_limit (now : Time) (maxWait : Option Time)
    (wake : Option Time) :
    (verdict now maxWait (.throttled wake)).statement = {} := by
  cases wake with
  | none => rfl
  | some wake => by_cases hw : withinMaxWait now wake maxWait <;> simp [verdict, hw]

theorem a_throttle_records_no_reset (now : Time) (maxWait : Option Time)
    (report : Report) (h : (verdict now maxWait report).verdict = .throttled) :
    (verdict now maxWait report).resetsAt = none := by
  cases report with
  | usageExhausted resetsAt statement => simp [verdict] at h
  | throttled wake =>
      cases wake with
      | none => rfl
      | some wake =>
          by_cases hw : withinMaxWait now wake maxWait <;> simp_all [verdict]

/-- A throttle is recorded as a usage limit exactly when its wake lies beyond
the cap the loop's retry policy holds. -/
theorem throttle_is_a_usage_limit_iff_beyond_max_wait (now wake : Time)
    (maxWait : Option Time) :
    (verdict now maxWait (.throttled (some wake))).verdict = .usageLimited ↔
      ¬ withinMaxWait now wake maxWait := by
  by_cases hw : withinMaxWait now wake maxWait <;> simp [verdict, hw]

/-- The recorded verdict and the loop's decision cannot differ: a throttle the
permit records as a usage limit, under the cap the loop holds, is one the loop
fails permanently whatever transport budget or deadline remains. -/
theorem recorded_usage_limit_is_the_loops_permanent_failure {s : State} {error : String}
    {wake : Time} (hphase : s.phase = .retracted .transport error wake)
    (h : (verdict s.now s.budget.maxWait (.throttled (some wake))).verdict = .usageLimited) :
    step? s .schedule = some { s with phase := .failedPermanent } :=
  transport_wait_beyond_max_wait_is_usage_limited hphase
    ((throttle_is_a_usage_limit_iff_beyond_max_wait _ _ _).mp h)

/-- A throttle the permit records as a throttle is one the loop does not fail
for its wait: the schedule step never fails it permanently. -/
theorem recorded_throttle_is_held_by_the_loop {s s' : State} {error : String}
    {wake : Time} (hphase : s.phase = .retracted .transport error wake)
    (h : (verdict s.now s.budget.maxWait (.throttled (some wake))).verdict = .throttled)
    (hstep : step? s .schedule = some s') : s'.phase ≠ .failedPermanent := by
  by_cases hw : withinMaxWait s.now wake s.budget.maxWait
  · simp only [step?, hphase, hw, not_true_eq_false, if_false] at hstep
    split at hstep <;> (simp only [Option.some.injEq] at hstep; subst s'; simp)
  · simp [verdict, hw] at h

end InferenceCall.ProviderLimit
