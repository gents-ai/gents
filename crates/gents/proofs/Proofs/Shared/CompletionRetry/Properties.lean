import Proofs.CompletionRetry.Transition

namespace CompletionRetry

theorem local_request_build_is_permanent_without_retry (s : State)
    (hphase : s.phase = .streaming) (error : String) (wake : Time) :
    step? s (.observeFailure FailureOrigin.localRequestBuild.class error wake) =
      some { s with phase := .failedPermanent } := by
  simp [FailureOrigin.class, step?, hphase]

theorem provider_stream_malformed_requires_retraction (s : State)
    (hphase : s.phase = .streaming) (error : String) (wake : Time) :
    step? s (.observeFailure FailureOrigin.providerStreamMalformed.class error wake) =
      some { s with phase := .retractRequired .transport error wake } := by
  simp [FailureOrigin.class, step?, hphase]

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

theorem retry_budget_caps_preserved {s s' : State} {action : Action}
    (htransport : s.transportUsed ≤ s.budget.transportRetries)
    (hresample : s.resampleUsed ≤ s.budget.resampleRetries)
    (h : step? s action = some s') :
    s'.transportUsed ≤ s'.budget.transportRetries ∧
      s'.resampleUsed ≤ s'.budget.resampleRetries := by
  cases action <;> simp only [step?] at h
  all_goals repeat' first | split at h
  all_goals try contradiction
  all_goals simp only [Option.some.injEq] at h
  all_goals subst s'
  all_goals simp_all <;> omega

theorem retry_trace_budget_caps_preserved {s s' : State}
    (htrace : Trace s s')
    (htransport : s.transportUsed ≤ s.budget.transportRetries)
    (hresample : s.resampleUsed ≤ s.budget.resampleRetries) :
    s'.transportUsed ≤ s'.budget.transportRetries ∧
      s'.resampleUsed ≤ s'.budget.resampleRetries := by
  induction htrace with
  | refl => exact ⟨htransport, hresample⟩
  | step transition rest ih =>
      obtain ⟨action, hstep⟩ := transition
      have hcaps := retry_budget_caps_preserved htransport hresample hstep
      exact ih hcaps.1 hcaps.2

theorem scheduled_backoff_fits_deadline {s s' : State} {wake : Time}
    (h : step? s .schedule = some s')
    (hbackoff : s'.phase = .backingOff wake) :
    fitsDeadline wake s.deadline ∧ s.now ≤ wake := by
  simp only [step?] at h
  split at h <;> try contradiction
  all_goals repeat' first | split at h
  all_goals try contradiction
  all_goals simp only [Option.some.injEq] at h
  all_goals subst s'
  all_goals simp_all

theorem successful_wake_uses_observed_time {s s' : State} {observed : Time}
    (h : step? s (.wake observed) = some s') :
    ∃ scheduled, s.phase = .backingOff scheduled ∧ scheduled ≤ observed ∧
      s.now ≤ observed ∧ fitsDeadline observed s.deadline ∧
      s'.phase = .issuing ∧ s'.now = observed := by
  cases hp : s.phase <;> simp [step?, hp] at h
  rename_i scheduled
  rcases h with ⟨bounds, rfl⟩
  exact ⟨scheduled, rfl, bounds.1, bounds.2.1, bounds.2.2, rfl, rfl⟩

theorem repair_issue_consumes_capability {s s' : State}
    (h : step? s .repairIssue = some s') :
    s.repairUsed = false ∧ s'.repairUsed = true ∧
      s'.attempt = s.attempt + 1 := by
  simp only [step?] at h
  split at h <;> try contradiction
  simp only [Option.some.injEq] at h
  subst s'
  simp_all

theorem repair_used_never_resets {s s' : State} {action : Action}
    (hused : s.repairUsed = true)
    (h : step? s action = some s') : s'.repairUsed = true := by
  cases action <;> simp only [step?] at h
  all_goals repeat' first | split at h
  all_goals try contradiction
  all_goals simp only [Option.some.injEq] at h
  all_goals subst s'
  all_goals simp_all

theorem repair_used_stays_used_across_trace {s s' : State}
    (htrace : Trace s s') (hused : s.repairUsed = true) :
    s'.repairUsed = true := by
  induction htrace with
  | refl => exact hused
  | step transition rest ih =>
      obtain ⟨action, hstep⟩ := transition
      exact ih (repair_used_never_resets hused hstep)

theorem used_repair_cannot_issue {s : State}
    (hused : s.repairUsed = true) : step? s .repairIssue = none := by
  simp [step?, hused]

theorem repair_issue_at_most_once {s s' : State}
    (h : step? s .repairIssue = some s') :
    step? s' .repairIssue = none := by
  have hused := repair_issue_consumes_capability h
  simp only [step?]
  simp [hused.2.1]

theorem repair_issue_at_most_once_across_trace {before after later : State}
    (hrepair : step? before .repairIssue = some after)
    (htrace : Trace after later) :
    step? later .repairIssue = none := by
  have hconsumed := repair_issue_consumes_capability hrepair
  apply used_repair_cannot_issue
  exact repair_used_stays_used_across_trace htrace hconsumed.2.1

theorem context_overflow_never_backs_off (s : State) (error : String) (wake : Time)
    (after : State) (hphase : s.phase = .retracted .contextOverflow error wake)
    (h : step? s .schedule = some after) :
    after.phase = .compacting ∨ after.phase = .exhausted := by
  simp only [step?, hphase] at h
  split at h <;> cases h <;> simp

theorem compact_issue_consumes_capability {s s' : State}
    (h : step? s .compactIssue = some s') :
    s.compactUsed = false ∧ s'.compactUsed = true ∧
      s'.attempt = s.attempt + 1 := by
  simp only [step?] at h
  split at h <;> try contradiction
  simp only [Option.some.injEq] at h
  subst s'
  simp_all

theorem compact_used_never_resets {s s' : State} {action : Action}
    (hused : s.compactUsed = true)
    (h : step? s action = some s') : s'.compactUsed = true := by
  cases action <;> simp only [step?] at h
  all_goals repeat' first | split at h
  all_goals try contradiction
  all_goals simp only [Option.some.injEq] at h
  all_goals subst s'
  all_goals simp_all

theorem compact_used_stays_used_across_trace {s s' : State}
    (htrace : Trace s s') (hused : s.compactUsed = true) :
    s'.compactUsed = true := by
  induction htrace with
  | refl => exact hused
  | step transition rest ih =>
      obtain ⟨action, hstep⟩ := transition
      exact ih (compact_used_never_resets hused hstep)

theorem compact_issue_at_most_once_across_trace {before after later : State}
    (hcompact : step? before .compactIssue = some after)
    (htrace : Trace after later) :
    step? later .compactIssue = none := by
  have hlater := compact_used_stays_used_across_trace htrace
    (compact_issue_consumes_capability hcompact).2.1
  simp [step?, hlater]

end CompletionRetry
