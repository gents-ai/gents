import Proofs.Shared.Basic

/-! Whether a provider's usage refusal still stops work on one backend and
model. The evidence is the calls themselves: the latest call the admission
permit recorded as `usage_limited` (`InferenceCall.ProviderLimit.Verdict`) and
the latest call on the same backend and model that completed. Calls are
ordered by `queued_at`, which admission writes once and never revises. -/
namespace InferenceCall.UsageRefusal

/-- The latest usage-limited call on one backend and model: when it was
queued, and the reset the provider reported for it. -/
structure Refusal where
  queuedAt : Time
  resetsAt : Option Time
  deriving DecidableEq, Repr

/-- Decision: a refusal stands until the reset the provider reported passes,
or until a call on the same backend and model, queued strictly after it,
completes. A completion queued at the same instant may have been served
before the refusal, so it does not clear it. A refusal whose reset the
provider did not report never lapses by time alone: only a later completion
shows the limit lifted. -/
def stands (now : Time) (refusal : Refusal) (lastCompletion : Option Time) : Bool :=
  (match refusal.resetsAt with
    | some reset => decide (now < reset)
    | none => true) &&
  (match lastCompletion with
    | some completed => decide (completed ≤ refusal.queuedAt)
    | none => true)

theorem a_later_completion_clears (now : Time) (refusal : Refusal) (completed : Time)
    (h : refusal.queuedAt < completed) : stands now refusal (some completed) = false := by
  unfold stands
  cases refusal.resetsAt <;> simp [Nat.not_le.mpr h]

theorem an_earlier_completion_does_not_clear (now : Time) (refusal : Refusal)
    (completed : Time) (h : completed ≤ refusal.queuedAt) :
    stands now refusal (some completed) = stands now refusal none := by
  simp [stands, h]

theorem a_passed_reset_clears (now reset queuedAt : Time) (lastCompletion : Option Time)
    (h : reset ≤ now) : stands now ⟨queuedAt, some reset⟩ lastCompletion = false := by
  simp [stands, Nat.not_lt.mpr h]

theorem an_unreported_reset_never_lapses (now queuedAt : Time) :
    stands now ⟨queuedAt, none⟩ none = true := rfl

end InferenceCall.UsageRefusal
