import Proofs.Session.Executable

/-!
# Session Queue Properties

Local invariants for the R4a session queue model.
-/

namespace SessionQueue

theorem canAppendAfter_true
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_after : canAppendAfter entries entry = true) :
    ∀ existing, existing ∈ entries → existing.createdAt ≤ entry.createdAt := by
  induction entries with
  | nil =>
      intro existing h_mem
      simp at h_mem
  | cons head tail ih =>
      by_cases h_head : head.createdAt ≤ entry.createdAt
      · simp [canAppendAfter, h_head] at h_after
        intro existing h_mem
        simp at h_mem
        rcases h_mem with h_eq | h_tail
        · rw [h_eq]
          exact h_head
        · exact ih h_after existing h_tail
      · simp [canAppendAfter, h_head] at h_after

theorem containsCoalescedQueueKey_false
    {entries : List QueueEntry}
    {source : QueueSource}
    {key : QueueKey}
    (h_missing : containsCoalescedQueueKey entries source key = false) :
    ∀ entry, entry ∈ entries → ¬ CoalescedKeyMatch entry source key := by
  induction entries with
  | nil =>
      intro entry h_mem
      simp at h_mem
  | cons head tail ih =>
      by_cases h_head : CoalescedKeyMatch head source key
      · simp [containsCoalescedQueueKey, h_head] at h_missing
      · simp [containsCoalescedQueueKey, h_head] at h_missing
        intro entry h_mem
        simp at h_mem
        rcases h_mem with h_eq | h_tail
        · rw [h_eq]
          exact h_head
        · exact ih h_missing entry h_tail

/-- An `Option RequestId` active slot cannot name two different active
    requests. This is the queue-level one-active-request invariant. -/
theorem active_at_most_one
    (s : SessionQueueState) :
    ∀ {rid₁ rid₂ : RequestId},
      s.active = some rid₁ → s.active = some rid₂ → rid₁ = rid₂ := by
  intro rid₁ rid₂ h₁ h₂
  rw [h₁] at h₂
  cases h₂
  rfl

theorem head_earliest_of_createdOrdered
    {entry : QueueEntry}
    {rest : List QueueEntry}
    (h_order : CreatedOrdered (entry :: rest)) :
    ∀ other, other ∈ entry :: rest → entry.createdAt ≤ other.createdAt := by
  intro other h_mem
  simp [CreatedOrdered] at h_order
  simp at h_mem
  rcases h_mem with h_eq | h_tail
  · rw [h_eq]
  · exact h_order.1 other h_tail

/-- A claim from a created-ordered pending list selects the earliest pending
    entry, because `claim_next` can only claim the list head. -/
theorem claim_next_selects_earliest
    {pre post : SessionQueueState}
    (h_active : pre.active = none)
    (h_order : CreatedOrdered pre.pending)
    {entry : QueueEntry}
    {rest : List QueueEntry}
    (h_pending : pre.pending = entry :: rest)
    (h_post : post = pre.claimHead entry rest) :
    Transition pre post ∧
    ∀ other, other ∈ pre.pending → entry.createdAt ≤ other.createdAt := by
  constructor
  · exact Transition.claim_next h_active h_pending h_post
  · rw [h_pending] at h_order ⊢
    exact head_earliest_of_createdOrdered h_order

theorem pending_head_earliest
    {pre : SessionQueueState}
    {entry : QueueEntry}
    {rest : List QueueEntry}
    (h_order : CreatedOrdered pre.pending)
    (h_pending : pre.pending = entry :: rest) :
    ∀ other, other ∈ pre.pending → entry.createdAt ≤ other.createdAt := by
  rw [h_pending] at h_order ⊢
  exact head_earliest_of_createdOrdered h_order

theorem createdOrdered_append_of_after
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_order : CreatedOrdered entries)
    (h_after : ∀ existing, existing ∈ entries → existing.createdAt ≤ entry.createdAt) :
    CreatedOrdered (entries ++ [entry]) := by
  induction entries with
  | nil =>
      simp [CreatedOrdered]
  | cons head tail ih =>
      simp [CreatedOrdered] at h_order ⊢
      constructor
      · intro other h_mem
        rcases h_mem with h_tail | h_eq
        · exact h_order.1 other h_tail
        · rw [h_eq]
          exact h_after head (by simp)
      · apply ih h_order.2
        intro existing h_existing
        exact h_after existing (by simp [h_existing])

theorem createdOrdered_append
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_order : CreatedOrdered entries)
    (h_after : canAppendAfter entries entry = true) :
    CreatedOrdered (entries ++ [entry]) :=
  createdOrdered_append_of_after h_order (canAppendAfter_true h_after)

theorem createdOrdered_tail
    {entry : QueueEntry}
    {rest : List QueueEntry}
    (h_order : CreatedOrdered (entry :: rest)) :
    CreatedOrdered rest := by
  simpa [CreatedOrdered] using h_order.2

theorem pendingAfterDrain_mem_original
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_mem : entry ∈ pendingAfterDrain source queueKey entries) :
    entry ∈ entries := by
  induction entries with
  | nil =>
      simp [pendingAfterDrain] at h_mem
  | cons head tail ih =>
      by_cases h_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [pendingAfterDrain, h_match] at h_mem
        exact by simp [ih h_mem]
      · simp [pendingAfterDrain, h_match] at h_mem
        rcases h_mem with h_eq | h_tail
        · exact by simp [h_eq]
        · exact by simp [ih h_tail]

theorem pendingAfterDrain_removes_matching
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry} :
    ∀ entry, entry ∈ pendingAfterDrain source queueKey entries →
      entry.matchesAutomatedWakeup source queueKey = false := by
  induction entries with
  | nil =>
      intro entry h_mem
      simp [pendingAfterDrain] at h_mem
  | cons head tail ih =>
      intro entry h_mem
      by_cases h_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [pendingAfterDrain, h_match] at h_mem
        exact ih entry h_mem
      · simp [pendingAfterDrain, h_match] at h_mem
        rcases h_mem with h_eq | h_tail
        · rw [h_eq]
          cases h_bool : head.matchesAutomatedWakeup source queueKey
          · rfl
          · exact absurd h_bool h_match
        · exact ih entry h_tail

theorem drainedRequestIds_contains_matching
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_mem : entry ∈ entries)
    (h_match : entry.matchesAutomatedWakeup source queueKey = true) :
    entry.requestId ∈ drainedRequestIds source queueKey entries := by
  revert entry
  induction entries with
  | nil =>
      intro entry h_mem _
      simp at h_mem
  | cons head tail ih =>
      intro entry h_mem h_match
      simp at h_mem
      by_cases h_head_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [drainedRequestIds, h_head_match]
        rcases h_mem with h_eq | h_tail
        · rw [h_eq]
          simp
        · exact Or.inr (ih h_tail h_match)
      · simp [drainedRequestIds, h_head_match]
        rcases h_mem with h_eq | h_tail
        · rw [h_eq] at h_match
          exact absurd h_match h_head_match
        · exact ih h_tail h_match

theorem pendingAfterDrain_preserves_nonmatching
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_mem : entry ∈ entries)
    (h_match : entry.matchesAutomatedWakeup source queueKey = false) :
    entry ∈ pendingAfterDrain source queueKey entries := by
  revert entry
  induction entries with
  | nil =>
      intro entry h_mem _
      simp at h_mem
  | cons head tail ih =>
      intro entry h_mem h_match
      simp at h_mem
      by_cases h_head_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [pendingAfterDrain, h_head_match]
        rcases h_mem with h_eq | h_tail
        · rw [h_eq] at h_match
          rw [h_head_match] at h_match
          cases h_match
        · exact ih h_tail h_match
      · simp [pendingAfterDrain, h_head_match]
        rcases h_mem with h_eq | h_tail
        · exact Or.inl h_eq
        · exact Or.inr (ih h_tail h_match)

theorem pendingAfterDrain_preserves_createdOrdered
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry}
    (h_order : CreatedOrdered entries) :
    CreatedOrdered (pendingAfterDrain source queueKey entries) := by
  induction entries with
  | nil =>
      simp [pendingAfterDrain, CreatedOrdered]
  | cons head tail ih =>
      simp [CreatedOrdered] at h_order
      by_cases h_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [pendingAfterDrain, h_match]
        exact ih h_order.2
      · simp [pendingAfterDrain, h_match, CreatedOrdered]
        constructor
        · intro other h_mem
          exact h_order.1 other (pendingAfterDrain_mem_original h_mem)
        · exact ih h_order.2

theorem uniqueCoalescedQueueKeys_append_append
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_unique : UniqueCoalescedQueueKeys entries)
    (h_policy : entry.policy = .append) :
    UniqueCoalescedQueueKeys (entries ++ [entry]) := by
  induction entries with
  | nil =>
      simp [UniqueCoalescedQueueKeys]
  | cons head tail ih =>
      simp [UniqueCoalescedQueueKeys] at h_unique ⊢
      constructor
      · intro source key h_head_source h_head_policy h_head_key other h_other
        rcases h_other with h_tail | h_entry
        · exact h_unique.1 source key h_head_source h_head_policy h_head_key other h_tail
        · rw [h_entry]
          intro h_match
          have h_bad : QueuePolicy.append = QueuePolicy.coalesce := by
            rw [← h_policy]
            exact h_match.2.1
          cases h_bad
      · exact ih h_unique.2

theorem uniqueCoalescedQueueKeys_append_fresh
    {entries : List QueueEntry}
    {entry : QueueEntry}
    {key : QueueKey}
    (h_unique : UniqueCoalescedQueueKeys entries)
    (h_key : entry.queueKey = some key)
    (h_missing : containsCoalescedQueueKey entries entry.source key = false) :
    UniqueCoalescedQueueKeys (entries ++ [entry]) := by
  induction entries with
  | nil =>
      simp [UniqueCoalescedQueueKeys]
  | cons head tail ih =>
      simp [UniqueCoalescedQueueKeys] at h_unique ⊢
      constructor
      · intro source key' h_head_source h_head_policy h_head_key other h_other
        rcases h_other with h_tail | h_entry
        · exact h_unique.1 source key' h_head_source h_head_policy h_head_key other h_tail
        · rw [h_entry]
          intro h_match
          have h_entry_source : entry.source = source := h_match.1
          have h_entry_key : key = key' := by
            have h_some : some key = some key' := by
              rw [← h_key]
              exact h_match.2.2
            exact Option.some.inj h_some
          have h_no_head :=
            containsCoalescedQueueKey_false h_missing head (by simp)
          exact h_no_head (by
            rw [h_entry_source, h_entry_key]
            exact ⟨h_head_source, h_head_policy, h_head_key⟩)
      · apply ih h_unique.2
        have h_tail_missing : containsCoalescedQueueKey tail entry.source key = false := by
          by_cases h_head : CoalescedKeyMatch head entry.source key
          · simp [containsCoalescedQueueKey, CoalescedKeyMatch, h_head.1, h_head.2.1, h_head.2.2] at h_missing
          · simp [containsCoalescedQueueKey, CoalescedKeyMatch] at h_missing
            by_cases h_source : head.source = entry.source
            · by_cases h_head_policy : head.policy = QueuePolicy.coalesce
              · have h_head_key : head.queueKey ≠ some key := by
                  intro h_head_key
                  exact h_head ⟨h_source, h_head_policy, h_head_key⟩
                simp [h_source, h_head_policy, h_head_key] at h_missing
                exact h_missing
              · simp [h_source, h_head_policy] at h_missing
                exact h_missing
            · simp [h_source] at h_missing
              exact h_missing
        exact h_tail_missing

theorem uniqueCoalescedQueueKeys_tail
    {entry : QueueEntry}
    {rest : List QueueEntry}
    (h_unique : UniqueCoalescedQueueKeys (entry :: rest)) :
    UniqueCoalescedQueueKeys rest := by
  simpa [UniqueCoalescedQueueKeys] using h_unique.2

theorem pendingAfterDrain_preserves_uniqueCoalescedQueueKeys
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry}
    (h_unique : UniqueCoalescedQueueKeys entries) :
    UniqueCoalescedQueueKeys (pendingAfterDrain source queueKey entries) := by
  induction entries with
  | nil =>
      simp [pendingAfterDrain, UniqueCoalescedQueueKeys]
  | cons head tail ih =>
      simp [UniqueCoalescedQueueKeys] at h_unique
      by_cases h_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [pendingAfterDrain, h_match]
        exact ih h_unique.2
      · simp [pendingAfterDrain, h_match, UniqueCoalescedQueueKeys]
        constructor
        · intro matchSource key h_head_source h_head_policy h_head_key other h_other
          exact h_unique.1 matchSource key h_head_source h_head_policy h_head_key other
            (pendingAfterDrain_mem_original h_other)
        · exact ih h_unique.2

/-- Coalescing a new keyed wake-up preserves coalesced queue-key uniqueness:
    the transition is only legal when that key is absent. -/
theorem coalesce_new_preserves_unique_coalesced_queueKeys
    {pre post : SessionQueueState}
    {entry : QueueEntry}
    {key : QueueKey}
    (h_unique : UniqueCoalescedQueueKeys pre.pending)
    (_h_policy : entry.policy = .coalesce)
    (h_key : entry.queueKey = some key)
    (h_missing : containsCoalescedQueueKey pre.pending entry.source key = false)
    (h_post : post = pre.appendPending entry) :
    UniqueCoalescedQueueKeys post.pending := by
  rw [h_post, SessionQueueState.appendPending]
  exact uniqueCoalescedQueueKeys_append_fresh h_unique h_key h_missing

/-- Coalescing an already-represented key is a no-op, so it cannot introduce a
    second pending entry for that key. -/
theorem coalesce_existing_preserves_unique_coalesced_queueKeys
    {pre post : SessionQueueState}
    (h_unique : UniqueCoalescedQueueKeys pre.pending)
    (h_post : post = pre) :
    UniqueCoalescedQueueKeys post.pending := by
  rw [h_post]
  exact h_unique

theorem transition_preserves_createdOrdered
    {pre post : SessionQueueState}
    (h_trans : Transition pre post)
    (h_order : CreatedOrdered pre.pending) :
    CreatedOrdered post.pending := by
  cases h_trans with
  | append_pending _ _ _ h_after h_post =>
      rw [h_post, SessionQueueState.appendPending]
      exact createdOrdered_append h_order h_after
  | coalesce_pending_new _ _ _ h_after h_post =>
      rw [h_post, SessionQueueState.appendPending]
      exact createdOrdered_append h_order h_after
  | coalesce_pending_existing _ _ h_post =>
      rw [h_post]
      exact h_order
  | claim_next _ h_pending h_post =>
      rw [h_pending] at h_order
      rw [h_post, SessionQueueState.claimHead]
      exact createdOrdered_tail h_order
  | finish_active _ h_post =>
      rw [h_post, SessionQueueState.finishActive]
      exact h_order
  | drain_automated _ h_post =>
      rw [h_post, SessionQueueState.drainAutomatedWakeups]
      exact pendingAfterDrain_preserves_createdOrdered h_order

theorem trace_preserves_createdOrdered
    {pre post : SessionQueueState}
    (h_trace : Trace pre post)
    (h_order : CreatedOrdered pre.pending) :
    CreatedOrdered post.pending := by
  induction h_trace with
  | refl =>
      exact h_order
  | step h_step _ ih =>
      exact ih (transition_preserves_createdOrdered h_step h_order)

theorem transition_preserves_uniqueCoalescedQueueKeys
    {pre post : SessionQueueState}
    (h_trans : Transition pre post)
    (h_unique : UniqueCoalescedQueueKeys pre.pending) :
    UniqueCoalescedQueueKeys post.pending := by
  cases h_trans with
  | append_pending h_policy _ _ _ h_post =>
      rw [h_post, SessionQueueState.appendPending]
      exact uniqueCoalescedQueueKeys_append_append h_unique h_policy
  | coalesce_pending_new h_well_formed _ h_missing _ h_post =>
      exact coalesce_new_preserves_unique_coalesced_queueKeys
        h_unique h_well_formed.2.1 h_well_formed.2.2 h_missing h_post
  | coalesce_pending_existing _ _ h_post =>
      exact coalesce_existing_preserves_unique_coalesced_queueKeys h_unique h_post
  | claim_next _ h_pending h_post =>
      rw [h_pending] at h_unique
      rw [h_post, SessionQueueState.claimHead]
      exact uniqueCoalescedQueueKeys_tail h_unique
  | finish_active _ h_post =>
      rw [h_post, SessionQueueState.finishActive]
      exact h_unique
  | drain_automated _ h_post =>
      rw [h_post, SessionQueueState.drainAutomatedWakeups]
      exact pendingAfterDrain_preserves_uniqueCoalescedQueueKeys h_unique

theorem trace_preserves_uniqueCoalescedQueueKeys
    {pre post : SessionQueueState}
    (h_trace : Trace pre post)
    (h_unique : UniqueCoalescedQueueKeys pre.pending) :
    UniqueCoalescedQueueKeys post.pending := by
  induction h_trace with
  | refl =>
      exact h_unique
  | step h_step _ ih =>
      exact ih (transition_preserves_uniqueCoalescedQueueKeys h_step h_unique)

/-- All queue transitions preserve existing terminal history. In particular,
    cancellation drains add terminalized automated wake-up ids instead of
    deleting prior terminal ids. -/
theorem terminal_history_monotonic
    {pre post : SessionQueueState}
    (h_trans : Transition pre post) :
    pre.terminal ⊆ post.terminal := by
  intro requestId h_mem
  cases h_trans with
  | append_pending _ _ _ _ h_post =>
      rw [h_post, SessionQueueState.appendPending]
      exact h_mem
  | coalesce_pending_new _ _ _ _ h_post =>
      rw [h_post, SessionQueueState.appendPending]
      exact h_mem
  | coalesce_pending_existing _ _ h_post =>
      rw [h_post]
      exact h_mem
  | claim_next _ _ h_post =>
      rw [h_post, SessionQueueState.claimHead]
      exact h_mem
  | finish_active _ h_post =>
      rw [h_post, SessionQueueState.finishActive]
      exact Finset.mem_insert_of_mem h_mem
  | drain_automated _ h_post =>
      rw [h_post, SessionQueueState.drainAutomatedWakeups]
      exact Finset.mem_union.mpr (Or.inl h_mem)

theorem trace_terminal_history_monotonic
    {pre post : SessionQueueState}
    (h_trace : Trace pre post) :
    pre.terminal ⊆ post.terminal := by
  intro requestId h_mem
  induction h_trace with
  | refl =>
      exact h_mem
  | step h_step _ ih =>
      exact ih (terminal_history_monotonic h_step h_mem)

theorem drainAutomated_preserves_terminal_history
    (pre : SessionQueueState)
    (source : QueueSource)
    (queueKey : Option QueueKey) :
    pre.terminal ⊆ (pre.drainAutomatedWakeups source queueKey).terminal := by
  intro requestId h_mem
  simp [SessionQueueState.drainAutomatedWakeups, h_mem]

theorem drainAutomated_removes_matching_from_pending
    (pre : SessionQueueState)
    (source : QueueSource)
    (queueKey : Option QueueKey) :
    ∀ entry, entry ∈ (pre.drainAutomatedWakeups source queueKey).pending →
      entry.matchesAutomatedWakeup source queueKey = false := by
  intro entry h_mem
  exact pendingAfterDrain_removes_matching entry h_mem

theorem drainAutomated_terminalizes_matching
    (pre : SessionQueueState)
    (source : QueueSource)
    (queueKey : Option QueueKey)
    {entry : QueueEntry}
    (h_mem : entry ∈ pre.pending)
    (h_match : entry.matchesAutomatedWakeup source queueKey = true) :
    entry.requestId ∈ (pre.drainAutomatedWakeups source queueKey).terminal := by
  simp [SessionQueueState.drainAutomatedWakeups]
  exact Or.inr (drainedRequestIds_contains_matching h_mem h_match)

theorem drainAutomated_preserves_nonmatching_pending
    (pre : SessionQueueState)
    (source : QueueSource)
    (queueKey : Option QueueKey)
    {entry : QueueEntry}
    (h_mem : entry ∈ pre.pending)
    (h_match : entry.matchesAutomatedWakeup source queueKey = false) :
    entry ∈ (pre.drainAutomatedWakeups source queueKey).pending :=
  pendingAfterDrain_preserves_nonmatching h_mem h_match

theorem step?_sound
    {pre post : SessionQueueState}
    {action : Action}
    (h_step : step? pre action = some post) :
    Transition pre post := by
  cases action with
  | appendPending entry =>
      by_cases h_guard :
        entry.policy = .append ∧
          entry.appendWellFormed ∧
          RequestIdFresh pre entry ∧
          canAppendAfter pre.pending entry = true
      · simp [step?, h_guard] at h_step
        cases h_step
        exact Transition.append_pending h_guard.1 h_guard.2.1 h_guard.2.2.1 h_guard.2.2.2 rfl
      · simp [step?, h_guard] at h_step
  | coalescePending entry =>
      cases h_key : entry.queueKey with
      | none =>
          simp [step?, h_key] at h_step
      | some key =>
          by_cases h_guard : entry.coalesceWellFormed key
          · by_cases h_contains : containsCoalescedQueueKey pre.pending entry.source key = true
            · simp [step?, h_key, h_guard, h_contains] at h_step
              cases h_step
              exact Transition.coalesce_pending_existing h_guard h_contains rfl
            · have h_missing : containsCoalescedQueueKey pre.pending entry.source key = false := by
                cases h_value : containsCoalescedQueueKey pre.pending entry.source key
                · rfl
                · exact absurd h_value h_contains
              by_cases h_fresh_after :
                RequestIdFresh pre entry ∧ canAppendAfter pre.pending entry = true
              · simp [step?, h_key, h_guard, h_contains, h_fresh_after] at h_step
                cases h_step
                exact Transition.coalesce_pending_new h_guard h_fresh_after.1 h_missing h_fresh_after.2 rfl
              · simp [step?, h_key, h_guard, h_contains, h_fresh_after] at h_step
          · simp [step?, h_key, h_guard] at h_step
  | claimNext =>
      cases h_active : pre.active with
      | none =>
          cases h_pending : pre.pending with
          | nil =>
              simp [step?, h_active, h_pending] at h_step
          | cons entry rest =>
              simp [step?, h_active, h_pending] at h_step
              cases h_step
              exact Transition.claim_next h_active h_pending rfl
      | some requestId =>
          simp [step?, h_active] at h_step
  | finishActive =>
      cases h_active : pre.active with
      | none =>
          simp [step?, h_active] at h_step
      | some requestId =>
          simp [step?, h_active] at h_step
          cases h_step
          exact Transition.finish_active h_active rfl
  | drainAutomated source queueKey =>
      by_cases h_source : source.automatedWakeup
      · simp [step?, h_source] at h_step
        cases h_step
        exact Transition.drain_automated h_source rfl
      · simp [step?, h_source] at h_step

theorem transition_complete
    {pre post : SessionQueueState}
    (h_trans : Transition pre post) :
    ∃ action : Action, step? pre action = some post := by
  match h_trans with
  | Transition.append_pending (entry := entry) h_policy h_well_formed h_fresh h_after h_post =>
      exact ⟨.appendPending entry, by
        simp [step?, h_policy, h_well_formed, h_fresh, h_after, h_post]⟩
  | Transition.coalesce_pending_new (entry := entry) (key := key) h_well_formed h_fresh h_missing h_after h_post =>
      exact ⟨.coalescePending entry, by
        rcases h_well_formed with ⟨h_source, h_policy, h_key⟩
        have h_missing_source :
            containsCoalescedQueueKey pre.pending QueueSource.subagentCompletion key = false := by
          simpa [h_source] using h_missing
        have h_fresh_after : RequestIdFresh pre entry ∧ canAppendAfter pre.pending entry = true :=
          ⟨h_fresh, h_after⟩
        simp [step?, QueueEntry.coalesceWellFormed, h_source, h_policy, h_key, h_missing,
          h_missing_source, h_fresh_after, h_post]⟩
  | Transition.coalesce_pending_existing (entry := entry) (key := key) h_well_formed h_contains h_post =>
      exact ⟨.coalescePending entry, by
        rcases h_well_formed with ⟨h_source, h_policy, h_key⟩
        have h_contains_source :
            containsCoalescedQueueKey pre.pending QueueSource.subagentCompletion key = true := by
          simpa [h_source] using h_contains
        simp [step?, QueueEntry.coalesceWellFormed, h_source, h_policy, h_key, h_contains,
          h_contains_source, h_post]⟩
  | Transition.claim_next h_active h_pending h_post =>
      exact ⟨.claimNext, by simp [step?, h_active, h_pending, h_post]⟩
  | Transition.finish_active h_active h_post =>
      exact ⟨.finishActive, by simp [step?, h_active, h_post]⟩
  | Transition.drain_automated (source := source) (queueKey := queueKey) h_source h_post =>
      exact ⟨.drainAutomated source queueKey, by simp [step?, h_source, h_post]⟩

theorem replay?_sound
    {start finish : SessionQueueState}
    {actions : List Action}
    (h_replay : replay? start actions = some finish) :
    Trace start finish := by
  induction actions generalizing start with
  | nil =>
      simp [replay?] at h_replay
      cases h_replay
      exact Trace.refl
  | cons action rest ih =>
      simp [replay?] at h_replay
      cases h_step : step? start action with
      | none =>
          simp [h_step] at h_replay
      | some next =>
          simp [h_step] at h_replay
          exact Trace.step (step?_sound h_step) (ih h_replay)

theorem trace_complete
    {pre post : SessionQueueState}
    (h_trace : Trace pre post) :
    ∃ actions : List Action, replay? pre actions = some post := by
  induction h_trace with
  | refl =>
      exact ⟨[], rfl⟩
  | step h_trans _ ih =>
      rcases transition_complete h_trans with ⟨action, h_action⟩
      rcases ih with ⟨actions, h_actions⟩
      refine ⟨action :: actions, ?_⟩
      simp [replay?, h_action, h_actions]

end SessionQueue
