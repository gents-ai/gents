import Proofs.Session.Executable

namespace SessionQueue

theorem pendingAfterDrainMatching_mem_original
    {source : QueueSource} {queueKey : Option QueueKey}
    {allowed : QueueEntry → Bool} {entries : List QueueEntry} {entry : QueueEntry}
    (h_mem : entry ∈ pendingAfterDrainMatching source queueKey allowed entries) :
    entry ∈ entries := by
  induction entries with
  | nil => simp [pendingAfterDrainMatching] at h_mem
  | cons head tail ih =>
      simp only [pendingAfterDrainMatching] at h_mem
      split at h_mem
      · exact List.mem_cons_of_mem _ (ih h_mem)
      · simp only [List.mem_cons] at h_mem
        rcases h_mem with h_eq | h_tail
        · simp [h_eq]
        · exact List.mem_cons_of_mem _ (ih h_tail)

private theorem pendingAfterDrainMatching_preserves_nonmatching
    {source : QueueSource} {queueKey : Option QueueKey}
    {allowed : QueueEntry → Bool} {entries : List QueueEntry} {entry : QueueEntry}
    (h_mem : entry ∈ entries)
    (h_not : (entry.matchesAutomatedWakeup source queueKey && allowed entry) = false) :
    entry ∈ pendingAfterDrainMatching source queueKey allowed entries := by
  induction entries with
  | nil => simp at h_mem
  | cons head tail ih =>
      simp only [List.mem_cons] at h_mem
      simp only [pendingAfterDrainMatching]
      split
      · rename_i h_branch
        rcases h_mem with h_eq | h_tail
        · subst head
          rw [h_branch] at h_not
          contradiction
        · exact ih h_tail
      · rcases h_mem with h_eq | h_tail
        · exact List.mem_cons.mpr (Or.inl h_eq)
        · exact List.mem_cons.mpr (Or.inr (ih h_tail))

private theorem drainedRequestIdsMatching_contains
    {source : QueueSource} {queueKey : Option QueueKey}
    {allowed : QueueEntry → Bool} {entries : List QueueEntry} {entry : QueueEntry}
    (h_mem : entry ∈ entries)
    (h_match : (entry.matchesAutomatedWakeup source queueKey && allowed entry) = true) :
    entry.requestId ∈ drainedRequestIdsMatching source queueKey allowed entries := by
  induction entries with
  | nil => simp at h_mem
  | cons head tail ih =>
      simp only [List.mem_cons] at h_mem
      simp only [drainedRequestIdsMatching]
      split
      · rcases h_mem with h_eq | h_tail
        · simp [h_eq]
        · exact Finset.mem_insert_of_mem (ih h_tail)
      · rename_i h_branch
        rcases h_mem with h_eq | h_tail
        · subst head
          exact False.elim (h_branch h_match)
        · exact ih h_tail

theorem drainObservedAutomated_preserves_terminal_history
    (pre : SessionQueueState) (source : QueueSource) (key : Option QueueKey)
    (observed : List RequestId) :
    pre.terminal ⊆ (pre.drainObservedAutomatedWakeups source key observed).terminal := by
  intro id h
  simp [SessionQueueState.drainObservedAutomatedWakeups, h]

theorem drainObservedAutomated_preserves_nonmatching_pending
    (pre : SessionQueueState) (source : QueueSource) (key : Option QueueKey)
    (observed : List RequestId) {entry : QueueEntry}
    (h_mem : entry ∈ pre.pending)
    (h_not : ¬(entry.matchesAutomatedWakeup source key = true ∧
      entry.requestId ∈ observed)) :
    entry ∈ (pre.drainObservedAutomatedWakeups source key observed).pending := by
  apply pendingAfterDrainMatching_preserves_nonmatching h_mem
  by_cases h_match : entry.matchesAutomatedWakeup source key = true
  · by_cases h_id : entry.requestId ∈ observed
    · exact False.elim (h_not ⟨h_match, h_id⟩)
    · simp [h_match, h_id]
  · cases h_bool : entry.matchesAutomatedWakeup source key
    · simp [h_bool]
    · exact False.elim (h_match h_bool)

theorem drainObservedAutomated_terminalizes_matching
    (pre : SessionQueueState) (source : QueueSource) (key : Option QueueKey)
    (observed : List RequestId) {entry : QueueEntry}
    (h_mem : entry ∈ pre.pending)
    (h_match : entry.matchesAutomatedWakeup source key = true ∧
      entry.requestId ∈ observed) :
    entry.requestId ∈ (pre.drainObservedAutomatedWakeups source key observed).terminal := by
  simp [SessionQueueState.drainObservedAutomatedWakeups]
  exact Or.inr (drainedRequestIdsMatching_contains h_mem (by simp [h_match]))

theorem pendingAfterDrain_mem_original
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_mem : entry ∈ pendingAfterDrain source queueKey entries) :
    entry ∈ entries := by
  induction entries with
  | nil =>
      simp at h_mem
  | cons head tail ih =>
      by_cases h_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [h_match] at h_mem
        exact by simp [ih h_mem]
      · simp [h_match] at h_mem
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
      simp at h_mem
  | cons head tail ih =>
      intro entry h_mem
      by_cases h_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [h_match] at h_mem
        exact ih entry h_mem
      · simp [h_match] at h_mem
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
      · simp [h_head_match]
        rcases h_mem with h_eq | h_tail
        · rw [h_eq]
          simp
        · exact Or.inr (ih h_tail h_match)
      · simp [h_head_match]
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
      · simp [h_head_match]
        rcases h_mem with h_eq | h_tail
        · rw [h_eq] at h_match
          rw [h_head_match] at h_match
          cases h_match
        · exact ih h_tail h_match
      · simp [h_head_match]
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
  | drain_observed_automated _ h_post =>
      rw [h_post, SessionQueueState.drainObservedAutomatedWakeups]
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
