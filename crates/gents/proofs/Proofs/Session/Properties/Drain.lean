import Proofs.Session.Executable

namespace SessionQueue

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

end SessionQueue
