import Proofs.Callback.Transition

namespace CallbackInvocation

def journalPrefixOk : List ActionJournalEntry → Bool
  | [] => true
  | [_] => true
  | a :: b :: rest =>
      (!ActionJournalState.laterThanValidated b.state ||
        decide (a.state = .resultDocsWritten)) &&
      journalPrefixOk (b :: rest)

def journalPrefix (journal : List ActionJournalEntry) : Prop :=
  journalPrefixOk journal = true

instance (journal : List ActionJournalEntry) : Decidable (journalPrefix journal) :=
  inferInstanceAs (Decidable (_ = true))

def resultEmittedOk (inv : CallbackInvocation) : Bool :=
  !inv.resultEmitted ||
    (decide (inv.state = .succeeded) &&
      inv.journal.all (fun e => decide (e.state = .resultDocsWritten)))

-- Denied never executes host actions. Failed after observe/docs may keep a
-- journal; it still must not emit CallbackResult.
def deniedFailedNoExecute (inv : CallbackInvocation) : Bool :=
  !decide (inv.state = .denied) || inv.journal.isEmpty

def invocationLegal (inv : CallbackInvocation) : Bool :=
  journalPrefixOk inv.journal && resultEmittedOk inv && deniedFailedNoExecute inv

def activeClaim (inv : CallbackInvocation) : Bool :=
  decide (inv.state = .claimed) || decide (inv.state = .running)

def countActive (ownerId invocationId : String) (invs : List CallbackInvocation) : Nat :=
  (invs.filter fun inv =>
      activeClaim inv &&
        decide (inv.ownerNodeDid = ownerId) &&
        decide (inv.invocationId = invocationId)).length

/-- Row-level invocation uniqueness, not a single-runtime/host-identity guarantee.
Concurrent use of one principal on multiple hosts is explicitly outside this model. -/
def ClaimUnique (invs : List CallbackInvocation) : Prop :=
  invs.all (fun inv => decide (countActive inv.ownerNodeDid inv.invocationId invs ≤ 1)) = true

instance (invs : List CallbackInvocation) : Decidable (ClaimUnique invs) :=
  inferInstanceAs (Decidable (_ = true))

theorem identity_fields_preserved
    {pre post : CallbackInvocation}
    (h : Transition pre post) :
    post.invocationId = pre.invocationId ∧
    post.ownerNodeDid = pre.ownerNodeDid := by
  cases h <;> simp_all

/-- Claim, execution and terminalization preserve the captured payload and
its group origin. No transition can replace them with live source contents. -/
theorem frozen_input_preserved {pre post : CallbackInvocation}
    (h : Transition pre post) :
    post.input = pre.input ∧ post.originGroupKey = pre.originGroupKey := by
  cases h <;> simp_all

theorem denied_or_failed_do_not_emit
    {pre post : CallbackInvocation}
    (h : Transition pre post)
    (hterm : post.state = .denied ∨ post.state = .failed) :
    post.resultEmitted = false := by
  cases h with
  | claim _ hpost =>
      simp [hpost] at hterm
  | run _ hpost =>
      simp [hpost] at hterm
  | succeed _ _ hpost =>
      simp [hpost] at hterm
  | fail _ hpost =>
      simp [hpost]
  | interrupt _ hpost =>
      simp [hpost]
  | deny_claimed _ _ hpost =>
      simp [hpost]
  | deny_running _ _ hpost =>
      simp [hpost]
  | retry _ _ hpost =>
      simp [hpost] at hterm

theorem denied_keeps_empty_journal
    {pre post : CallbackInvocation}
    (h : Transition pre post)
    (hden : post.state = .denied) :
    post.journal = [] := by
  cases h with
  | claim _ hpost =>
      simp [hpost] at hden
  | run _ hpost =>
      simp [hpost] at hden
  | succeed _ _ hpost =>
      simp [hpost] at hden
  | fail _ hpost =>
      simp [hpost] at hden
  | interrupt _ hpost =>
      simp [hpost] at hden
  | deny_claimed _ hjournal hpost =>
      simp [hpost, hjournal]
  | deny_running _ hjournal hpost =>
      simp [hpost, hjournal]
  | retry _ _ hpost =>
      simp [hpost] at hden

/-- Failure keeps the journal, or marks its executing actions interrupted when
recovery cut the attempt off; it never drops what an action did. -/
theorem fail_preserves_journal
    {pre post : CallbackInvocation}
    (h : Transition pre post)
    (hfail : post.state = .failed) :
    (post.journal = pre.journal ∨ post.journal = interruptJournal pre.journal) ∧
      post.resultEmitted = false := by
  cases h with
  | claim _ hpost =>
      simp [hpost] at hfail
  | run _ hpost =>
      simp [hpost] at hfail
  | succeed _ _ hpost =>
      simp [hpost] at hfail
  | fail _ hpost =>
      simp [hpost]
  | interrupt _ hpost =>
      simp [hpost]
  | deny_claimed _ _ hpost =>
      simp [hpost] at hfail
  | deny_running _ _ hpost =>
      simp [hpost] at hfail
  | retry _ _ hpost =>
      simp [hpost] at hfail

theorem result_emitted_only_on_success
    (inv : CallbackInvocation)
    (h : resultEmittedOk inv = true)
    (hemitted : inv.resultEmitted = true) :
    inv.state = .succeeded ∧
      ∀ e ∈ inv.journal, e.state = .resultDocsWritten := by
  simp [resultEmittedOk, hemitted] at h
  exact h

theorem journal_prefix_blocks_early_execute :
    journalPrefixOk
      [{ index := 0, state := .validated }, { index := 1, state := .executing }] = false := by
  native_decide

theorem journal_prefix_allows_written_then_executing :
    journalPrefixOk
      [{ index := 0, state := .resultDocsWritten }, { index := 1, state := .executing }] =
      true := by
  native_decide

/-- A retry starts over from pending with nothing recorded and no result. -/
theorem retry_starts_clean {pre post : CallbackInvocation}
    (h : Transition pre post) (hpre : pre.state = .failed) (_hpost : post.state = .pending) :
    post.journal = [] ∧ post.resultEmitted = false := by
  cases h with
  | claim hp _ => simp [hpre] at hp
  | run hp _ => simp [hpre] at hp
  | succeed hp _ _ => simp [hpre] at hp
  | fail hp _ => simp [hpre] at hp
  | interrupt hp _ => simp [hpre] at hp
  | deny_claimed hp _ _ => simp [hpre] at hp
  | deny_running hp _ _ => simp [hpre] at hp
  | retry _ _ heq => simp [heq]

/-- Nothing that observed an effect or wrote results is ever run again. -/
theorem retry_never_repeats_an_effect (inv : CallbackInvocation) (maxAttempts : Nat)
    (h : retryAllowed inv maxAttempts = true) :
    ∀ e ∈ inv.journal, ActionJournalState.effectful e.state = false := by
  simp [retryAllowed] at h
  exact h.2

/-- An attempt cut off mid-run is never run again, whatever the attempt budget:
the runtime cannot observe what external side effect it had, and repeating an
unknown effect is unsafe. -/
theorem interrupted_attempt_never_retried (inv : CallbackInvocation) (maxAttempts : Nat)
    (h : ∃ e ∈ inv.journal, e.state = .interrupted) :
    retryAllowed inv maxAttempts = false := by
  obtain ⟨e, he, hs⟩ := h
  cases hr : retryAllowed inv maxAttempts with
  | false => rfl
  | true =>
      have := retry_never_repeats_an_effect inv maxAttempts hr e he
      simp [hs, ActionJournalState.effectful] at this

/-- A failed invocation with an interrupted action takes no further step: it
stays failed. -/
theorem interrupted_invocation_is_final {inv post : CallbackInvocation}
    (hfailed : inv.state = .failed) (h : ∃ e ∈ inv.journal, e.state = .interrupted) :
    ¬ Transition inv post := by
  intro step
  cases step with
  | claim hp _ => simp [hfailed] at hp
  | run hp _ => simp [hfailed] at hp
  | succeed hp _ _ => simp [hfailed] at hp
  | fail hp _ => simp [hfailed] at hp
  | interrupt hp _ => simp [hfailed] at hp
  | deny_claimed hp _ _ => simp [hfailed] at hp
  | deny_running hp _ _ => simp [hfailed] at hp
  | retry m hr _ =>
      have := interrupted_attempt_never_retried inv m h
      simp [this] at hr

/-- Recovery of a cut-off attempt is the model's `interrupt` step. -/
theorem recover_steps_by_interrupt (inv : CallbackInvocation)
    (hrun : inv.state = .running) (hjournal : inv.journal ≠ []) :
    Transition inv (recover inv) :=
  .interrupt hrun (by simp [recover, hrun, hjournal])

/-- Recovery never leaves a cut-off attempt that had started an action where a
retry could run it again, whatever the attempt budget. -/
theorem recovered_attempt_never_retried (inv : CallbackInvocation) (maxAttempts : Nat)
    (hrun : inv.state = .running) (hexec : ∃ e ∈ inv.journal, e.state = .executing) :
    retryAllowed (recover inv) maxAttempts = false := by
  obtain ⟨e, he, hs⟩ := hexec
  have hjournal : inv.journal ≠ [] := by
    intro hnil
    simp [hnil] at he
  apply interrupted_attempt_never_retried
  refine ⟨{ e with state := .interrupted }, ?_, rfl⟩
  simp only [recover, hrun, hjournal, ne_eq, not_false_eq_true, and_self, if_true,
    interruptJournal, List.mem_map]
  exact ⟨e, he, by simp [hs, ActionJournalState.markInterrupted]⟩

/-- A denial of a running invocation whose actions started is recovery. -/
theorem deny_started_is_recover (inv : CallbackInvocation)
    (hjournal : inv.journal ≠ []) : deny inv = recover inv := by
  simp [deny, hjournal]

/-- A denial of a running invocation is a model step: `deny_running` before any
action started, `interrupt` after. -/
theorem deny_steps (inv : CallbackInvocation) (hrun : inv.state = .running) :
    Transition inv (deny inv) := by
  by_cases hjournal : inv.journal = []
  · exact .deny_running hrun hjournal (by simp [deny, hjournal])
  · rw [deny_started_is_recover inv hjournal]
    exact recover_steps_by_interrupt inv hrun hjournal

/-- A denial never leaves a started attempt where a retry could run it again,
so re-enabling a disabled callback cannot repeat an interrupted action. -/
theorem denied_attempt_never_retried (inv : CallbackInvocation) (maxAttempts : Nat)
    (hrun : inv.state = .running) (hexec : ∃ e ∈ inv.journal, e.state = .executing) :
    retryAllowed (deny inv) maxAttempts = false := by
  have hjournal : inv.journal ≠ [] := by
    obtain ⟨e, he, _⟩ := hexec
    intro hnil
    simp [hnil] at he
  rw [deny_started_is_recover inv hjournal]
  exact recovered_attempt_never_retried inv maxAttempts hrun hexec

/-- Retries are bounded by the attempt budget. -/
theorem retry_is_bounded (inv : CallbackInvocation) (maxAttempts : Nat)
    (h : retryAllowed inv maxAttempts = true) : inv.attempts < maxAttempts := by
  simp [retryAllowed] at h
  exact h.1.2

/-- Only a failed invocation is retried; a success or a denial is final. -/
theorem retry_only_after_failure (inv : CallbackInvocation) (maxAttempts : Nat)
    (h : retryAllowed inv maxAttempts = true) : inv.state = .failed := by
  simp [retryAllowed] at h
  exact h.1.1

theorem claim_unique_nil : ClaimUnique [] := by
  simp [ClaimUnique]

theorem succeeded_not_pending : InvocationState.succeeded ≠ .pending := by
  decide

end CallbackInvocation
