import Proofs.Session.Properties.Drain

/-! Created-time ordering and earliest-claim properties for session queues. -/

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

end SessionQueue
