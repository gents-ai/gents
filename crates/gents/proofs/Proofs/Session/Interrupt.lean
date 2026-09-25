import Proofs.Session.Properties
import Proofs.Request.State

namespace SessionQueue

private def drainWakeKeys (observed : List RequestId)
    (queue : SessionQueueState) : List QueueEntry → SessionQueueState
  | [] => queue
  | entry :: rest =>
      drainWakeKeys observed
        (queue.drainObservedAutomatedWakeups .backgroundCompletion entry.queueKey observed) rest

private theorem drainWakeKeys_trace (observed : List RequestId)
    (queue : SessionQueueState) (entries : List QueueEntry) :
    Trace queue (drainWakeKeys observed queue entries) := by
  induction entries generalizing queue with
  | nil => exact .refl
  | cons entry rest ih =>
      exact .step (.drain_observed_automated (by trivial) rfl) (ih _)

private theorem drainWakeKeys_active (observed : List RequestId)
    (queue : SessionQueueState) (entries : List QueueEntry) :
    (drainWakeKeys observed queue entries).active = queue.active := by
  induction entries generalizing queue with
  | nil => rfl
  | cons entry rest ih =>
      simp only [drainWakeKeys, ih, SessionQueueState.drainObservedAutomatedWakeups]

private theorem drainWakeKeys_terminal_monotonic
    (observed : List RequestId) (queue : SessionQueueState) (entries : List QueueEntry) :
    queue.terminal ⊆ (drainWakeKeys observed queue entries).terminal :=
  trace_terminal_history_monotonic (drainWakeKeys_trace observed queue entries)

private theorem drainWakeKeys_preserves_entry
    (observed : List RequestId) (queue : SessionQueueState)
    (entries : List QueueEntry) (entry : QueueEntry)
    (h_nonwake : entry.source ≠ .backgroundCompletion ∨ entry.origin = .interactive)
    (h_pending : entry ∈ queue.pending) :
    entry ∈ (drainWakeKeys observed queue entries).pending := by
  induction entries generalizing queue with
  | nil => exact h_pending
  | cons head rest ih =>
      apply ih
      · exact drainObservedAutomated_preserves_nonmatching_pending queue
          .backgroundCompletion head.queueKey observed h_pending (by
            rcases h_nonwake with h_source | h_origin
            · cases head.queueKey <;>
                simp [QueueEntry.matchesAutomatedWakeup, h_source]
            · cases head.queueKey <;>
                simp [QueueEntry.matchesAutomatedWakeup, h_origin])

private theorem drainWakeKeys_preserves_unobserved
    (observed : List RequestId) (queue : SessionQueueState)
    (entries : List QueueEntry) (entry : QueueEntry)
    (h_unobserved : entry.requestId ∉ observed)
    (h_pending : entry ∈ queue.pending) :
    entry ∈ (drainWakeKeys observed queue entries).pending := by
  induction entries generalizing queue with
  | nil => exact h_pending
  | cons head rest ih =>
      apply ih
      exact drainObservedAutomated_preserves_nonmatching_pending queue
        .backgroundCompletion head.queueKey observed h_pending
        (fun h => h_unobserved h.2)

private theorem drainWakeKeys_pending_or_terminal
    (observed : List RequestId) (queue : SessionQueueState)
    (entries : List QueueEntry) (entry : QueueEntry)
    (h : entry ∈ queue.pending ∨ entry.requestId ∈ queue.terminal) :
    entry ∈ (drainWakeKeys observed queue entries).pending ∨
      entry.requestId ∈ (drainWakeKeys observed queue entries).terminal := by
  induction entries generalizing queue with
  | nil => exact h
  | cons head rest ih =>
      apply ih
      rcases h with h_pending | h_terminal
      · by_cases h_match : entry.matchesAutomatedWakeup
            .backgroundCompletion head.queueKey = true ∧ entry.requestId ∈ observed
        · exact Or.inr (drainObservedAutomated_terminalizes_matching queue
            .backgroundCompletion head.queueKey observed h_pending h_match)
        · exact Or.inl (drainObservedAutomated_preserves_nonmatching_pending queue
            .backgroundCompletion head.queueKey observed h_pending h_match)
      · exact Or.inr (drainObservedAutomated_preserves_terminal_history queue
          .backgroundCompletion head.queueKey observed h_terminal)

private theorem drainWakeKeys_terminalizes_wake
    (observed : List RequestId) (queue : SessionQueueState)
    (entries : List QueueEntry) (entry : QueueEntry)
    (h_in : entry ∈ entries) (h_pending : entry ∈ queue.pending)
    (h_observed : entry.requestId ∈ observed)
    (h_source : entry.source = .backgroundCompletion)
    (h_origin : entry.origin = .scheduled)
    (h_policy : entry.policy = .coalesce)
    (key : QueueKey) (h_key : entry.queueKey = some key) :
    entry.requestId ∈ (drainWakeKeys observed queue entries).terminal := by
  induction entries generalizing queue with
  | nil => simp at h_in
  | cons head rest ih =>
      simp only [List.mem_cons] at h_in
      rcases h_in with h_head | h_rest
      · subst head
        have h_match : entry.matchesAutomatedWakeup
            .backgroundCompletion entry.queueKey = true := by
          simp [QueueEntry.matchesAutomatedWakeup, QueueEntry.coalesceWellFormed,
            h_source, h_origin, h_policy, h_key, QueueSource.automatedWakeup]
        have h_terminal := drainObservedAutomated_terminalizes_matching queue
          .backgroundCompletion entry.queueKey observed h_pending ⟨h_match, h_observed⟩
        exact drainWakeKeys_terminal_monotonic observed _ rest h_terminal
      · have h_next := drainWakeKeys_pending_or_terminal observed queue [head] entry
          (Or.inl h_pending)
        simp only [drainWakeKeys] at h_next
        rcases h_next with h_still | h_terminal
        · exact ih _ h_rest h_still
        · exact drainWakeKeys_terminal_monotonic observed _ rest h_terminal

/-- A first latch drains only physical queue identities observed by its earlier
pending-row scan. A concurrently committed unobserved wake may survive even if
its commit precedes this latch commit; all wakes committed after the latch must
survive. Replay cannot widen the observed set. The native adapter must retain
the exact authenticated session scope across capture and commit. The pinned
Regolith store does not detect empty-scan phantoms, including at Serializable;
DefraDB's HTTP transaction begin exposes only the read-only option. The local
write gate orders local completion publication, but an overlapping HTTP latch
may miss a completion inserted after its pending scan. -/
def latchInterruptObserved (request : RequestContext)
    (observed : List RequestId) (queue : SessionQueueState) :
    RequestContext × SessionQueueState :=
  if request.interruptRequestedAt.isSome then (request, queue)
  else ({ request with interruptRequestedAt := some request.currentTime },
    drainWakeKeys observed queue queue.pending)

/-- Sequential first latch captures the whole current pending queue. -/
def latchInterrupt (request : RequestContext) (queue : SessionQueueState) :
    RequestContext × SessionQueueState :=
  latchInterruptObserved request (queue.pending.map QueueEntry.requestId) queue

def latchInterruptObservedScoped (target : AgentSession.Scope)
    (request : RequestContext) (observed : List RequestId)
    (queue : SessionQueueState) : Option (RequestContext × SessionQueueState) :=
  if queue.scope = target then some (latchInterruptObserved request observed queue) else none

def latchInterruptScoped (target : AgentSession.Scope) (request : RequestContext)
    (queue : SessionQueueState) : Option (RequestContext × SessionQueueState) :=
  latchInterruptObservedScoped target request
    (queue.pending.map QueueEntry.requestId) queue

theorem latchInterrupt_replay (request : RequestContext) (queue : SessionQueueState)
    (latched : request.interruptRequestedAt.isSome = true) :
    latchInterrupt request queue = (request, queue) := by
  simp [latchInterrupt, latchInterruptObserved, latched]

theorem latchInterrupt_sets_intent (request : RequestContext) (queue : SessionQueueState) :
    (latchInterrupt request queue).1.interruptRequestedAt.isSome = true := by
  unfold latchInterrupt
  unfold latchInterruptObserved
  split <;> simp_all

theorem latchInterruptObserved_queue_trace (request : RequestContext)
    (observed : List RequestId) (queue : SessionQueueState) :
    Trace queue (latchInterruptObserved request observed queue).2 := by
  unfold latchInterruptObserved
  split
  · exact .refl
  · exact drainWakeKeys_trace observed queue queue.pending

theorem latchInterruptObserved_preserves_active (request : RequestContext)
    (observed : List RequestId) (queue : SessionQueueState) :
    (latchInterruptObserved request observed queue).2.active = queue.active := by
  unfold latchInterruptObserved
  split
  · rfl
  · exact drainWakeKeys_active observed queue queue.pending

theorem latchInterruptObserved_preserves_terminal_history
    (request : RequestContext) (observed : List RequestId)
    (queue : SessionQueueState) :
    queue.terminal ⊆ (latchInterruptObserved request observed queue).2.terminal :=
  trace_terminal_history_monotonic
    (latchInterruptObserved_queue_trace request observed queue)

theorem latchInterrupt_queue_trace (request : RequestContext) (queue : SessionQueueState) :
    Trace queue (latchInterrupt request queue).2 :=
  latchInterruptObserved_queue_trace request _ queue

theorem latchInterrupt_preserves_active
    (request : RequestContext) (queue : SessionQueueState) :
    (latchInterrupt request queue).2.active = queue.active :=
  latchInterruptObserved_preserves_active request _ queue

theorem latchInterrupt_preserves_terminal_history
    (request : RequestContext) (queue : SessionQueueState) :
    queue.terminal ⊆ (latchInterrupt request queue).2.terminal :=
  latchInterruptObserved_preserves_terminal_history request _ queue

theorem latchInterrupt_first_preserves_nonwake
    (request : RequestContext) (queue : SessionQueueState) (entry : QueueEntry)
    (h_first : request.interruptRequestedAt = none)
    (h_nonwake : entry.source ≠ .backgroundCompletion)
    (h_pending : entry ∈ queue.pending) :
    entry ∈ (latchInterrupt request queue).2.pending := by
  simp [latchInterrupt, latchInterruptObserved, h_first]
  exact drainWakeKeys_preserves_entry _ queue queue.pending entry (Or.inl h_nonwake) h_pending

/-- Interactive work remains pending even when its source metadata resembles an
automated background wake. -/
theorem latchInterruptObserved_preserves_interactive
    (request : RequestContext) (queue : SessionQueueState)
    (observed : List RequestId) (entry : QueueEntry)
    (h_origin : entry.origin = .interactive)
    (h_pending : entry ∈ queue.pending) :
    entry ∈ (latchInterruptObserved request observed queue).2.pending := by
  unfold latchInterruptObserved
  split
  · exact h_pending
  · exact drainWakeKeys_preserves_entry observed queue queue.pending entry
      (Or.inr h_origin) h_pending

theorem latchInterrupt_first_terminalizes_wake
    (request : RequestContext) (queue : SessionQueueState) (entry : QueueEntry)
    (h_first : request.interruptRequestedAt = none)
    (h_pending : entry ∈ queue.pending)
    (h_source : entry.source = .backgroundCompletion)
    (h_origin : entry.origin = .scheduled)
    (h_policy : entry.policy = .coalesce)
    (key : QueueKey) (h_key : entry.queueKey = some key) :
    entry.requestId ∈ (latchInterrupt request queue).2.terminal := by
  simp [latchInterrupt, latchInterruptObserved, h_first]
  exact drainWakeKeys_terminalizes_wake _ queue queue.pending entry h_pending
    h_pending (List.mem_map.mpr ⟨entry, h_pending, rfl⟩) h_source h_origin
    h_policy key h_key

theorem latchInterruptObserved_preserves_unobserved
    (request : RequestContext) (queue : SessionQueueState)
    (observed : List RequestId) (entry : QueueEntry)
    (h_unobserved : entry.requestId ∉ observed)
    (h_pending : entry ∈ queue.pending) :
    entry ∈ (latchInterruptObserved request observed queue).2.pending := by
  unfold latchInterruptObserved
  split
  · exact h_pending
  · exact drainWakeKeys_preserves_unobserved observed queue queue.pending entry
      h_unobserved h_pending

theorem latchInterruptObserved_first_terminalizes_observed_wake
    (request : RequestContext) (queue : SessionQueueState)
    (observed : List RequestId) (entry : QueueEntry)
    (h_first : request.interruptRequestedAt = none)
    (h_pending : entry ∈ queue.pending)
    (h_observed : entry.requestId ∈ observed)
    (h_source : entry.source = .backgroundCompletion)
    (h_origin : entry.origin = .scheduled)
    (h_policy : entry.policy = .coalesce)
    (key : QueueKey) (h_key : entry.queueKey = some key) :
    entry.requestId ∈ (latchInterruptObserved request observed queue).2.terminal := by
  simp [latchInterruptObserved, h_first]
  exact drainWakeKeys_terminalizes_wake observed queue queue.pending entry
    h_pending h_pending h_observed h_source h_origin h_policy key h_key

theorem latchInterruptObserved_replay_preserves_later_queue
    (request : RequestContext) (before later : SessionQueueState)
    (observedBefore observedLater : List RequestId) :
    latchInterruptObserved
        (latchInterruptObserved request observedBefore before).1 observedLater later =
      ((latchInterruptObserved request observedBefore before).1, later) := by
  have h_latched :
      (latchInterruptObserved request observedBefore before).1.interruptRequestedAt.isSome =
        true := by
    unfold latchInterruptObserved
    split <;> simp_all
  let latched := (latchInterruptObserved request observedBefore before).1
  change latchInterruptObserved latched observedLater later = (latched, later)
  change latched.interruptRequestedAt.isSome = true at h_latched
  unfold latchInterruptObserved
  simp [h_latched]

/-- Replay after any intervening completion retains that entire newer queue,
including completions coalesced under a pre-existing key. -/
theorem latchInterrupt_replay_preserves_later_queue
    (request : RequestContext) (before later : SessionQueueState) :
    latchInterrupt (latchInterrupt request before).1 later =
      ((latchInterrupt request before).1, later) :=
  latchInterrupt_replay _ _ (latchInterrupt_sets_intent request before)

theorem latchInterrupt_foreign_scope (target : AgentSession.Scope)
    (request : RequestContext) (queue : SessionQueueState) (foreign : queue.scope ≠ target) :
    latchInterruptScoped target request queue = none := by
  simp [latchInterruptScoped, latchInterruptObservedScoped, foreign]

theorem latchInterruptObserved_foreign_scope (target : AgentSession.Scope)
    (request : RequestContext) (observed : List RequestId)
    (queue : SessionQueueState) (foreign : queue.scope ≠ target) :
    latchInterruptObservedScoped target request observed queue = none := by
  simp [latchInterruptObservedScoped, foreign]

end SessionQueue
