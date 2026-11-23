import Proofs.Session.SessionQueue.Properties.Drain
import Proofs.Session.SessionQueue.Steering

namespace SessionQueue

theorem canAppendAfter_true
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_after : canAppendAfter entries entry = true) :
    ∀ existing, existing ∈ entries → existing.orderKey ≤ entry.orderKey := by
  induction entries with
  | nil =>
      intro existing h_mem
      simp at h_mem
  | cons head tail ih =>
      by_cases h_head : head.orderKey ≤ entry.orderKey
      · simp [canAppendAfter, h_head] at h_after
        intro existing h_mem
        simp at h_mem
        rcases h_mem with h_eq | h_tail
        · rw [h_eq]
          exact h_head
        · exact ih h_after existing h_tail
      · simp [canAppendAfter, h_head] at h_after

theorem head_earliest_of_createdOrdered
    {entry : QueueEntry}
    {rest : List QueueEntry}
    (h_order : CreatedOrdered (entry :: rest)) :
    ∀ other, other ∈ entry :: rest → entry.orderKey ≤ other.orderKey := by
  intro other h_mem
  simp [CreatedOrdered] at h_order
  simp at h_mem
  rcases h_mem with h_eq | h_tail
  · rw [h_eq]
  · exact h_order.1 other h_tail

theorem claim_next_selects_earliest
    {pre post : SessionQueueState}
    (h_active : pre.active = none)
    (h_order : CreatedOrdered pre.pending)
    {entry : QueueEntry}
    {rest : List QueueEntry}
    (h_pending : pre.pending = entry :: rest)
    (h_post : post = pre.claimHead entry rest) :
    Transition pre post ∧
    ∀ other, other ∈ pre.pending → entry.orderKey ≤ other.orderKey := by
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
    ∀ other, other ∈ pre.pending → entry.orderKey ≤ other.orderKey := by
  rw [h_pending] at h_order ⊢
  exact head_earliest_of_createdOrdered h_order

theorem createdOrdered_append_of_after
    {entries : List QueueEntry}
    {entry : QueueEntry}
    (h_order : CreatedOrdered entries)
    (h_after : ∀ existing, existing ∈ entries → existing.orderKey ≤ entry.orderKey) :
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

theorem createdOrdered_drop
    {entries : List QueueEntry} (count : Nat)
    (h_order : CreatedOrdered entries) :
    CreatedOrdered (entries.drop count) := by
  induction entries generalizing count with
  | nil => simpa using h_order
  | cons head tail ih =>
      cases count with
      | zero => simpa using h_order
      | succ count =>
          simpa using ih count (createdOrdered_tail h_order)

theorem pendingAfterDrain_preserves_createdOrdered
    {source : QueueSource}
    {queueKey : Option QueueKey}
    {entries : List QueueEntry}
    (h_order : CreatedOrdered entries) :
    CreatedOrdered (pendingAfterDrain source queueKey entries) := by
  induction entries with
  | nil =>
      simp [CreatedOrdered]
  | cons head tail ih =>
      simp [CreatedOrdered] at h_order
      by_cases h_match : head.matchesAutomatedWakeup source queueKey = true
      · simp [h_match]
        exact ih h_order.2
      · simp [h_match, CreatedOrdered]
        constructor
        · intro other h_mem
          exact h_order.1 other (pendingAfterDrain_mem_original h_mem)
        · exact ih h_order.2

theorem pendingAfterDrainMatching_preserves_createdOrdered
    {source : QueueSource} {queueKey : Option QueueKey}
    {allowed : QueueEntry → Bool} {entries : List QueueEntry}
    (h_order : CreatedOrdered entries) :
    CreatedOrdered (pendingAfterDrainMatching source queueKey allowed entries) := by
  induction entries with
  | nil => simp [pendingAfterDrainMatching, CreatedOrdered]
  | cons head tail ih =>
      simp [CreatedOrdered] at h_order
      simp only [pendingAfterDrainMatching]
      split
      · exact ih h_order.2
      · simp only [CreatedOrdered]
        constructor
        · intro other h_mem
          exact h_order.1 other (pendingAfterDrainMatching_mem_original h_mem)
        · exact ih h_order.2

theorem createdOrdered_sublist
    {entries smaller : List QueueEntry}
    (h_sub : smaller.Sublist entries)
    (h_order : CreatedOrdered entries) :
    CreatedOrdered smaller := by
  induction h_sub with
  | slnil => exact h_order
  | cons head _ ih => exact ih (createdOrdered_tail h_order)
  | cons₂ head h_rest ih =>
      simp only [CreatedOrdered] at h_order ⊢
      exact ⟨fun other h_mem => h_order.1 other (h_rest.subset h_mem), ih h_order.2⟩

theorem pendingAfterDrainMatching_sublist
    (source : QueueSource) (queueKey : Option QueueKey)
    (allowed : QueueEntry → Bool) (entries : List QueueEntry) :
    (pendingAfterDrainMatching source queueKey allowed entries).Sublist entries := by
  induction entries with
  | nil => exact List.Sublist.slnil
  | cons head tail ih =>
      simp only [pendingAfterDrainMatching]
      split
      · exact ih.cons head
      · exact ih.cons₂ head

theorem createdOrdered_iff_pairwise (entries : List QueueEntry) :
    CreatedOrdered entries ↔ entries.Pairwise (fun a b => createdLe a b = true) := by
  induction entries with
  | nil => simp [CreatedOrdered]
  | cons entry rest ih => simp [CreatedOrdered, List.pairwise_cons, createdLe, ih]

theorem createdOrdered_mergeCreated {xs ys : List QueueEntry}
    (h_xs : CreatedOrdered xs) (h_ys : CreatedOrdered ys) :
    CreatedOrdered (mergeCreated xs ys) := by
  rw [createdOrdered_iff_pairwise] at h_xs h_ys ⊢
  exact List.sorted_merge
    (fun a b c hab hbc => by
      simp only [createdLe, decide_eq_true_eq] at hab hbc ⊢
      exact Nat.le_trans hab hbc)
    (fun a b => by
      simp only [createdLe, Bool.or_eq_true, decide_eq_true_eq]
      exact Nat.le_total _ _) xs ys h_xs h_ys

theorem createdOrdered_append_le {xs ys : List QueueEntry}
    (h_order : CreatedOrdered (xs ++ ys)) :
    ∀ x ∈ xs, ∀ y ∈ ys, x.orderKey ≤ y.orderKey := by
  rw [createdOrdered_iff_pairwise, List.pairwise_append] at h_order
  intro x hx y hy
  simpa [createdLe] using h_order.2.2 x hx y hy

theorem queued_perm (s : SessionQueueState) : s.queued.Perm (s.folding ++ s.pending) :=
  List.merge_perm_append _

theorem mem_queued {s : SessionQueueState} {entry : QueueEntry} :
    entry ∈ s.queued ↔ entry ∈ s.folding ∨ entry ∈ s.pending := by
  rw [(queued_perm s).mem_iff, List.mem_append]

private theorem foldRun_steers {head : QueueEntry} {admitted : List RequestId}
    {entries : List QueueEntry} {entry : QueueEntry}
    (h : entry ∈ foldRun head admitted entries) : steersInto head entry := by
  induction entries with
  | nil => simp [foldRun] at h
  | cons first rest ih =>
      by_cases hf : head.foldsInto first ∧ first.requestId ∈ admitted
      · simp only [foldRun, hf, and_self, ↓reduceIte, List.mem_cons] at h
        rcases h with rfl | h
        · obtain ⟨-, -, ⟨hsource, hpolicy, horigin, -, hdelivery⟩, hreq, hctx, hfresh, hgoal⟩ := hf.1
          exact ⟨hfresh, hdelivery, hpolicy, Or.inl hsource, horigin, hreq, hctx, hgoal⟩
        · exact ih h
      · simp [foldRun, hf] at h

/-- The queue order every transition keeps: the active turn's selection and
the rest of the queue are each in created order; every selected entry steers
into the active request; and a pending entry created before a selected one is
an ordinary queued turn that the selection passed. -/
def QueueOrdered (q : SessionQueueState) : Prop :=
  CreatedOrdered q.folding ∧ CreatedOrdered q.pending ∧
    (q.folding ≠ [] → ∃ head : QueueEntry, q.active = some head.requestId ∧
      ∀ folded ∈ q.folding, steersInto head folded) ∧
    (∀ kept ∈ q.pending, ∀ folded ∈ q.folding,
      kept.orderKey < folded.orderKey → kept.delivery = .queue)

theorem queueOrdered_queued {q : SessionQueueState} (h : QueueOrdered q) :
    CreatedOrdered q.queued :=
  createdOrdered_mergeCreated h.1 h.2.1

theorem idle_queue_has_no_selection {q : SessionQueueState} (h : QueueOrdered q)
    (h_idle : q.active = none) : q.folding = [] := by
  by_contra hne
  obtain ⟨head, hhead, -⟩ := h.2.2.1 hne
  simp [h_idle] at hhead

private theorem appended_queueOrdered {pre : SessionQueueState} {entry : QueueEntry}
    (h : QueueOrdered pre) (h_after : canAppendAfter (pre.folding ++ pre.pending) entry = true) :
    QueueOrdered (pre.appendPending entry) := by
  obtain ⟨h_fold, h_pend, h_steer, h_queue⟩ := h
  have h_le := canAppendAfter_true h_after
  refine ⟨h_fold, ?_, h_steer, ?_⟩
  · exact createdOrdered_append_of_after h_pend
      (fun existing hx => h_le existing (List.mem_append_right _ hx))
  · intro kept hkept folded hfolded hlt
    simp only [SessionQueueState.appendPending, List.mem_append, List.mem_singleton] at hkept
    rcases hkept with hkept | rfl
    · exact h_queue kept hkept folded hfolded hlt
    · exact absurd hlt (Nat.not_lt.mpr (h_le folded (List.mem_append_left _ hfolded)))

theorem transition_preserves_createdOrdered
    {pre post : SessionQueueState}
    (h_trans : Transition pre post)
    (h_order : QueueOrdered pre) :
    QueueOrdered post := by
  cases h_trans with
  | append_pending _ _ _ h_after h_post =>
      rw [h_post]
      exact appended_queueOrdered h_order h_after
  | coalesce_pending_new _ _ _ h_after h_post =>
      rw [h_post]
      exact appended_queueOrdered h_order h_after
  | coalesce_pending_existing _ _ h_post =>
      rw [h_post]
      exact h_order
  | claim_next h_active h_pending h_post =>
      have h_empty := idle_queue_has_no_selection h_order h_active
      obtain ⟨-, h_pend, -, -⟩ := h_order
      rw [h_pending] at h_pend
      rw [h_post]
      refine ⟨by simp [SessionQueueState.claimHead, h_empty, CreatedOrdered],
        createdOrdered_tail h_pend, ?_, ?_⟩
      · intro hne
        simp [SessionQueueState.claimHead, h_empty] at hne
      · intro kept _ folded hfolded
        simp [SessionQueueState.claimHead, h_empty] at hfolded
  | @claim_folding entry rest admitted _ h_pending h_post =>
      obtain ⟨-, h_pend, -, -⟩ := h_order
      rw [h_pending] at h_pend
      have h_rest := createdOrdered_tail h_pend
      have h_split : foldRun entry admitted rest ++
          rest.drop (foldRun entry admitted rest).length = rest := foldRun_append_drop _ _ _
      have h_rest' := h_rest
      rw [← h_split] at h_rest'
      rw [h_post]
      refine ⟨(createdOrdered_sublist (List.sublist_append_left _ _) h_rest' :
          CreatedOrdered (foldRun entry admitted rest)),
        (createdOrdered_drop _ h_rest :
          CreatedOrdered (rest.drop (foldRun entry admitted rest).length)), ?_, ?_⟩
      · intro _
        exact ⟨entry, rfl, fun folded hfolded => foldRun_steers hfolded⟩
      · intro kept hkept folded hfolded hlt
        exact absurd hlt (Nat.not_lt.mpr (createdOrdered_append_le h_rest' folded hfolded kept hkept))
  | consume_folded _ h_folding h_post =>
      obtain ⟨h_fold, h_pend, h_steer, h_queue⟩ := h_order
      rw [h_post]
      rw [h_folding] at h_fold h_steer h_queue
      refine ⟨createdOrdered_tail h_fold, h_pend, ?_, ?_⟩
      · intro _
        obtain ⟨head, hhead, hall⟩ := h_steer (List.cons_ne_nil _ _)
        exact ⟨head, hhead, fun folded hfolded => hall folded (List.mem_cons_of_mem _ hfolded)⟩
      · intro kept hkept folded hfolded hlt
        exact h_queue kept hkept folded (List.mem_cons_of_mem _ hfolded) hlt
  | finish_active _ h_post =>
      rw [h_post]
      refine ⟨by simp [SessionQueueState.finishActive, CreatedOrdered],
        queueOrdered_queued h_order, ?_, ?_⟩
      · intro hne
        exact absurd rfl hne
      · intro kept _ folded hfolded
        simp [SessionQueueState.finishActive] at hfolded
  | drain_automated _ h_post =>
      obtain ⟨h_fold, h_pend, h_steer, h_queue⟩ := h_order
      rw [h_post]
      exact ⟨h_fold, pendingAfterDrain_preserves_createdOrdered h_pend, h_steer,
        fun kept hkept => h_queue kept (pendingAfterDrain_mem_original hkept)⟩
  | drain_observed_automated _ h_post =>
      obtain ⟨h_fold, h_pend, h_steer, h_queue⟩ := h_order
      rw [h_post]
      exact ⟨h_fold, pendingAfterDrainMatching_preserves_createdOrdered h_pend, h_steer,
        fun kept hkept => h_queue kept (pendingAfterDrainMatching_mem_original hkept)⟩

theorem trace_preserves_createdOrdered
    {pre post : SessionQueueState}
    (h_trace : Trace pre post)
    (h_order : QueueOrdered pre) :
    QueueOrdered post := by
  induction h_trace with
  | refl =>
      exact h_order
  | step h_step _ ih =>
      exact ih (transition_preserves_createdOrdered h_step h_order)

end SessionQueue
