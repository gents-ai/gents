import Proofs.CompletionRetry.Transition

namespace CompletionRetry

theorem retry_schedule_requires_retracted {s s' : State}
    (h : step? s .schedule = some s')
    (_hretry : ∃ wake, s'.phase = .backingOff wake) :
    ∃ failure error wake, s.phase = .retracted failure error wake := by
  cases hp : s.phase <;> simp [step?, hp] at h
  all_goals try contradiction
  all_goals
    repeat' first | split at h
    all_goals simp_all

theorem accepted_cannot_require_retraction {s : State} {header : Nat}
    (h : s.phase = .accepted header) :
    ∀ failure error wake, step? s (.observeFailure failure error wake) = none := by
  intro failure error wake
  simp [step?, h]

theorem accepted_cannot_schedule_retry {s : State} {header : Nat}
    (h : s.phase = .accepted header) : step? s .schedule = none := by
  simp [step?, h]

theorem accepted_tool_failure_never_resamples {s s' : State} {header : Nat}
    (hphase : s.phase = .accepted header)
    (h : step? s .acceptedToolFailure = some s') :
    s'.phase = .acceptedToolFailed header ∧
      s'.transportUsed = s.transportUsed ∧
      s'.resampleUsed = s.resampleUsed ∧
      s'.attempt = s.attempt := by
  simp [step?, hphase] at h
  subst s'
  exact ⟨rfl, rfl, rfl, rfl⟩

theorem usage_monotone {s s' : State} {action : Action}
    (h : step? s action = some s') : s.usageCharged ≤ s'.usageCharged := by
  cases action <;> simp only [step?] at h
  all_goals repeat' first | split at h
  all_goals try contradiction
  all_goals simp only [Option.some.injEq] at h
  all_goals subst s'
  all_goals simp

theorem retraction_preserves_usage {s s' : State}
    (h : step? s (.confirmRetraction true) = some s') :
    s'.usageCharged = s.usageCharged := by
  simp only [step?] at h
  split at h <;> try contradiction
  cases h
  rfl

theorem acceptance_preserves_usage {s s' : State} {header : Nat}
    (h : step? s (.accept header) = some s') :
    s'.usageCharged = s.usageCharged := by
  simp only [step?] at h
  split at h <;> try contradiction
  cases h
  rfl

theorem retry_budgets_monotone {s s' : State} {action : Action}
    (h : step? s action = some s') :
    s.transportUsed ≤ s'.transportUsed ∧ s.resampleUsed ≤ s'.resampleUsed := by
  cases action <;> simp only [step?] at h
  all_goals repeat' first | split at h
  all_goals try contradiction
  all_goals simp only [Option.some.injEq] at h
  all_goals subst s'
  all_goals simp <;> omega

end CompletionRetry
