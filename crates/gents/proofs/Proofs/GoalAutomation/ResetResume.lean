import Proofs.GoalAutomation.OperatorResume

/-! #2121: a second cause for the existing resume transaction. A usage-limited
Goal the operator opted in may resume by itself once the reset its provider
reported has passed. The transaction, its predecessor key and its child are
`resume` unchanged; this file only adds the guard a timer resume must pass.
`Request.authorized` keeps its meaning: the timer still runs the owner and
canonical parent checks, acting as the node with the owner's identity.
The operator cause is `resume` itself, so operator resume is untouched.
-/
namespace GoalAutomation.OperatorResume

/-- Facts a timer resume reads inside its resume transaction. Times are
seconds on one clock. `resetAt` is the provider's absolute reset time recorded
on the failed call that ended the latest request (`none`: no reset reported).
`limitStartedAt` is that call's start. `profileNamesAccount` means the profile
that served the call still selects the call's backend, and the serving sign-in
connected before the limited call (the same sign-in). `accountEnabled` is the
serving account's state. `now` is the resume's clock. -/
structure ResetFacts where
  optedIn : Bool
  resetAt : Option Nat
  limitStartedAt : Nat
  now : Nat
  profileNamesAccount : Bool
  accountEnabled : Bool
  deriving DecidableEq, Repr

/-- A timer resume is due: opted in, same account and enabled, and a reported
reset that is later than the limited call's start and has passed. A reset no
later than the call's start is stale and never due. There is no cap on how far
away the reset may be. -/
def ResetFacts.due (f : ResetFacts) : Bool :=
  f.optedIn && f.profileNamesAccount && f.accountEnabled &&
    match f.resetAt with
    | some t => decide (f.limitStartedAt < t) && decide (t ≤ f.now)
    | none => false

inductive Cause where
  | operator
  | resetReached (facts : ResetFacts)
  deriving DecidableEq, Repr

/-- Resume by cause. A reset cause only moves a `usageLimited` Goal, and waits
(`deferred`) until its facts are due; then it is the same `resume`. -/
def resumeBy (s : Snapshot) (cause : Cause) (r : Request) (commit : Bool) :
    Snapshot × Outcome :=
  match cause with
  | .operator => resume s r commit
  | .resetReached f =>
      if s.goal.status ≠ .usageLimited then (s, .illegal)
      else if !f.due then (s, .deferred)
      else resume s r commit

theorem operator_cause_is_resume (s : Snapshot) (r : Request) (commit : Bool) :
    resumeBy s .operator r commit = resume s r commit := rfl

theorem reset_resume_only_from_usage_limited (s : Snapshot) (f : ResetFacts)
    (r : Request) (commit : Bool) (h : s.goal.status ≠ .usageLimited) :
    resumeBy s (.resetReached f) r commit = (s, .illegal) := by
  simp [resumeBy, h]

theorem reset_resume_not_due_is_noop (s : Snapshot) (f : ResetFacts)
    (r : Request) (commit : Bool) (h : f.due = false) :
    (resumeBy s (.resetReached f) r commit).1 = s := by
  unfold resumeBy
  by_cases hs : s.goal.status = .usageLimited <;> simp [hs, h]

theorem no_reset_resume_without_opt_in (s : Snapshot) (f : ResetFacts)
    (r : Request) (commit : Bool) (h : f.optedIn = false) :
    (resumeBy s (.resetReached f) r commit).1 = s :=
  reset_resume_not_due_is_noop s f r commit (by simp [ResetFacts.due, h])

theorem no_reset_resume_without_reported_reset (s : Snapshot) (f : ResetFacts)
    (r : Request) (commit : Bool) (h : f.resetAt = none) :
    (resumeBy s (.resetReached f) r commit).1 = s :=
  reset_resume_not_due_is_noop s f r commit (by simp [ResetFacts.due, h])

theorem no_reset_resume_before_reset (s : Snapshot) (f : ResetFacts) (t : Nat)
    (r : Request) (commit : Bool) (h : f.resetAt = some t) (early : f.now < t) :
    (resumeBy s (.resetReached f) r commit).1 = s :=
  reset_resume_not_due_is_noop s f r commit
    (by simp [ResetFacts.due, h, Nat.not_le.mpr early])

theorem no_reset_resume_after_account_change (s : Snapshot) (f : ResetFacts)
    (r : Request) (commit : Bool)
    (h : f.profileNamesAccount = false ∨ f.accountEnabled = false) :
    (resumeBy s (.resetReached f) r commit).1 = s :=
  reset_resume_not_due_is_noop s f r commit
    (by rcases h with h | h <;> simp [ResetFacts.due, h])

theorem no_reset_resume_for_a_stale_reset (s : Snapshot) (f : ResetFacts) (t : Nat)
    (r : Request) (commit : Bool) (h : f.resetAt = some t)
    (stale : t ≤ f.limitStartedAt) :
    (resumeBy s (.resetReached f) r commit).1 = s :=
  reset_resume_not_due_is_noop s f r commit
    (by simp [ResetFacts.due, h, Nat.not_lt.mpr stale])

theorem reset_created_requires_due (s : Snapshot) (f : ResetFacts)
    (r : Request) (commit : Bool)
    (h : (resumeBy s (.resetReached f) r commit).2 = .created) :
    f.due = true ∧ s.goal.status = .usageLimited := by
  unfold resumeBy at h
  by_cases hs : s.goal.status = .usageLimited
  · by_cases hd : f.due = true
    · exact ⟨hd, hs⟩
    · simp [hs, hd] at h
  · simp [hs] at h

theorem reset_resume_preserves_budget_and_usage (s : Snapshot) (cause : Cause)
    (r : Request) (commit : Bool) :
    (resumeBy s cause r commit).1.tokensUsed = s.tokensUsed ∧
    (resumeBy s cause r commit).1.tokenBudget = s.tokenBudget := by
  unfold resumeBy
  split
  · exact resume_preserves_budget_and_usage s r commit
  · split
    · exact ⟨rfl, rfl⟩
    · split
      · exact ⟨rfl, rfl⟩
      · exact resume_preserves_budget_and_usage s r commit

/-- After a timer resume committed, the Goal is active, so any later reset
cause against that state, the same retry included, publishes nothing. -/
theorem reset_retry_after_commit_is_noop (s : Snapshot) (f f' : ResetFacts)
    (r r' : Request) (commit commit' : Bool)
    (h : (resumeBy s (.resetReached f) r commit).2 = .created) :
    resumeBy (resumeBy s (.resetReached f) r commit).1 (.resetReached f') r' commit' =
      ((resumeBy s (.resetReached f) r commit).1, .illegal) := by
  have due := reset_created_requires_due s f r commit h
  have same : resumeBy s (.resetReached f) r commit = resume s r commit := by
    simp [resumeBy, due.1, due.2]
  rw [same] at h ⊢
  have active := (created_publishes_atomically s r commit h).1
  exact reset_resume_only_from_usage_limited _ f' r' commit' (by simp [active])

theorem due_reset_bounds (f : ResetFacts) (t : Nat) (h : f.due = true)
    (ht : f.resetAt = some t) : f.limitStartedAt < t ∧ t ≤ f.now := by
  simp [ResetFacts.due, ht] at h
  exact ⟨h.2.1, h.2.2⟩

/-- Successive timer resumes need strictly later resets. Premise, not proved
here: a child published by a timer resume starts its calls no earlier than that
resume, so a second limit's call starts at or after the first resume's clock
(`f₁.now ≤ f₂.limitStartedAt`). With the predecessor key (one child per stopped
request) this gives at most one timer resume per reported reset; a provider
that repeats a stale reset gets none, and the Goal waits for the operator. -/
theorem reset_times_strictly_increase (f₁ f₂ : ResetFacts) (t₁ t₂ : Nat)
    (h₁ : f₁.due = true) (ht₁ : f₁.resetAt = some t₁)
    (h₂ : f₂.due = true) (ht₂ : f₂.resetAt = some t₂)
    (order : f₁.now ≤ f₂.limitStartedAt) : t₁ < t₂ := by
  have b₁ := due_reset_bounds f₁ t₁ h₁ ht₁
  have b₂ := due_reset_bounds f₂ t₂ h₂ ht₂
  omega

end GoalAutomation.OperatorResume
