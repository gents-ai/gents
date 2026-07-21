import Proofs.Session.Properties.Drain

/-! Coalesced queue-key uniqueness preservation for session queues. -/

namespace SessionQueue

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

end SessionQueue
