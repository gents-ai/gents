import Proofs.Triggers.SerialSupport

/-!
# Serial Trigger Theorems

Public T2 forms for serial trigger at-most-one behavior.
-/

/--
T2 (serial at-most-one): any reachable system state has at most one
non-terminal request per trigger tuple, provided every request for that
tuple in the state uses `.serial` concurrency.

**Hypothesis framing**: the hypothesis is about the CURRENT state's
requests, not the whole trace. This matches the original spec framing
("the system state at this instant"). A future refinement may strengthen
this to a pre-trace invariant (tracked as a follow-up).

Stated over `Reachable s` — a hand-crafted `SystemState` with multiple
parallel/latestOnly fires would violate the raw bound; this theorem is
correctly about states reachable via `dispatchStep`/`lifecycleTerminateStep`.
-/
theorem T2_serial_at_most_one
    (s : SystemState) (t : TriggerKey) (h_reach : Reachable s) :
    (∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial) →
    s.nonTerminalCountFor t ≤ 1 := by
  induction h_reach with
  | empty =>
    intro _
    rw [nonTerminalCountFor_empty]
    exact Nat.zero_le 1
  | step s' snap intent h_prev ih =>
    intro h_hyp_post
    have h_hyp_pre := dispatchStep_hypothesis_preservation s' snap intent t h_hyp_post
    have h_before := ih h_hyp_pre
    cases h_conc : intent.concurrency with
    | serial =>
      exact dispatchStep_serial_bounds_count s' snap intent t h_conc h_before
    | parallel =>
      have h_monotone :=
        dispatchStep_parallel_count_eq s' snap intent t h_conc h_hyp_post
      exact Nat.le_trans h_monotone h_before
    | latestOnly =>
      have h_monotone :=
        dispatchStep_latestOnly_count_le s' snap intent t h_conc h_hyp_post
      exact Nat.le_trans h_monotone h_before
  | terminate s' reqId h_prev ih =>
    intro h_hyp_post
    -- Convert post-state hypothesis to pre-state via
    -- lifecycleTerminateStep_preserves_causedBy_and_concurrency.
    have h_hyp_pre : ∀ r ∈ s'.requests, r.causedBy = some t → r.concurrency = .serial := by
      intro r h_mem h_causedBy
      obtain ⟨r', h_mem', h_cb, h_conc⟩ :=
        lifecycleTerminateStep_preserves_causedBy_and_concurrency s' reqId r h_mem
      have h_causedBy' : r'.causedBy = some t := h_cb.trans h_causedBy
      exact h_conc ▸ h_hyp_post r' h_mem' h_causedBy'
    have h_before := ih h_hyp_pre
    calc (lifecycleTerminateStep s' reqId).nonTerminalCountFor t
        ≤ s'.nonTerminalCountFor t := lifecycleTerminateStep_preserves_bound s' reqId t
      _ ≤ 1 := h_before

/--
T2 lifted to an admissibility-constrained trigger trace relation.

This is the preferred theorem shape for downstream trigger proofs: prove the
trace boundary once via `ReachableUnder`, then reuse the existing T2 result by
forgetting back to the raw operational semantics.
-/
theorem T2_serial_at_most_one_under
    (P : FireIntent → Prop)
    (s : SystemState)
    (t : TriggerKey)
    (h_reach : ReachableUnder P s) :
    (∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial) →
    s.nonTerminalCountFor t ≤ 1 :=
  T2_serial_at_most_one s t (ReachableUnder.toReachable h_reach)

/-- T2 stated over the boundary-tightened `WellFormedReachable` relation. -/
theorem T2_serial_at_most_one_wellFormed
    (s : SystemState)
    (t : TriggerKey)
    (h_reach : WellFormedReachable s) :
    (∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial) →
    s.nonTerminalCountFor t ≤ 1 :=
  T2_serial_at_most_one_under FireIntent.WellFormed s t h_reach

/--
Stronger pre-trace form of T2: if the trigger trace is well-formed and every
intent targeting `t` is serial at the boundary, then the reachable state has
at most one non-terminal request for `t`.
-/
theorem T2_serial_at_most_one_pretrace
    (s : SystemState)
    (t : TriggerKey)
    (h_reach : SeriallyReachable t s) :
    s.nonTerminalCountFor t ≤ 1 :=
  T2_serial_at_most_one_under
    (fun intent => intent.WellFormed ∧ intent.SerialForKey t)
    s
    t
    h_reach
    (seriallyReachable_requests_for_key_are_serial s t h_reach)
