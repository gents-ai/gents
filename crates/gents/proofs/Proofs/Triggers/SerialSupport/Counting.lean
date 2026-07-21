import Proofs.Triggers.Reachability

/-!
# Serial Trigger Counting Helpers

Counting lemmas used by the public serial-trigger theorems.
-/

/--
Helper: if `l.any p = false`, then `(l.filter p).length = 0`.

This is the "if no element matches, filter is empty" observation, stated
at the level of Bool predicates. Used by `dispatchStep_serial_bounds_count`
to collapse the pre-append count when the serial skip predicate said there
was no in-flight match.
-/
private theorem list_any_false_filter_length_zero
    {α : Type} {p : α → Bool} {l : List α}
    (h : l.any p = false) : (l.filter p).length = 0 := by
  induction l with
  | nil => simp
  | cons hd tl ih =>
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    obtain ⟨h_hd, h_tl⟩ := h
    -- p hd = false, so filter strips hd and recurses.
    rw [List.filter_cons_of_neg (by rw [h_hd]; decide)]
    exact ih h_tl

/--
Serial-arm preservation of T2's bound.

When `dispatchStep` is invoked with `.serial` concurrency, the post-state
non-terminal count for tuple `t` is bounded by 1, given the pre-state count
is bounded by 1.

Case analysis on `dispatch` / `seed.causedByTriggerId` / `t`-match / any-match:
- `dispatch = none`: state unchanged, bound from hypothesis.
- Serial, `key = none`: append; new request has `causedBy = none`, which
  never matches `some t`, so the count is unchanged.
- Serial, `key = some k` with `k ≠ t`: new request's `causedBy = some k ≠ some t`,
  so the count is unchanged.
- Serial, `key = some t`, any-match true: state unchanged (skip branch),
  bound from hypothesis.
- Serial, `key = some t`, any-match false: the old count is 0 (by
  `list_any_false_filter_length_zero`), append one matching non-terminal,
  new count = 1.
-/
theorem dispatchStep_serial_bounds_count
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) (t : TriggerKey)
    (h_serial : intent.concurrency = .serial)
    (h_before : s.nonTerminalCountFor t ≤ 1) :
    (dispatchStep s snap intent).nonTerminalCountFor t ≤ 1 := by
  unfold dispatchStep
  cases h_disp : dispatch snap intent with
  | none =>
    simp only
    exact h_before
  | some seed =>
    simp only
    rw [h_serial]
    simp only
    -- Let p := predicate for the filter count
    set p : AgentRequest → Bool :=
      (fun r => (r.causedBy == some t) && !r.isTerminal) with hp_def
    cases h_key : seed.causedByTriggerId with
    | none =>
      -- key = none: append newRequest with causedBy = none.
      simp only
      -- Post-state: s.requests ++ [newRequest], newRequest.causedBy = none.
      -- p newRequest = ((none == some t) && ...) = false.
      have h_pred_false : p { id := s!"dispatched-{s.requests.length}",
                              causedBy := none,
                              concurrency := intent.concurrency,
                              isTerminal := false,
                              executionOrigin :=
                                match seed.causedByTriggerKind with
                                | .manual => .interactive
                                | .schedule | .event => .scheduled } = false := by
        simp [hp_def]
      show (List.filter p (s.requests ++ [_])).length ≤ 1
      rw [List.filter_append, List.filter_cons_of_neg (by rw [h_pred_false]; decide),
          List.filter_nil, List.append_nil]
      exact h_before
    | some k =>
      -- key = some k. Branch on whether the full tuple (k, seed.kind) equals t.
      simp only
      by_cases h_eq : (k, seed.causedByTriggerKind) = t
      · -- key = some t: any-match case analysis.
        by_cases h_any :
          s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                     && !r.isTerminal) = true
        · -- Skip branch: state unchanged.
          rw [if_pos h_any]
          exact h_before
        · -- Append branch: old count = 0 (by helper), new count = 1.
          rw [if_neg h_any]
          -- h_any : ¬ (any = true), so any = false.
          have h_any_false :
              s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                         && !r.isTerminal) = false := by
            cases h : s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                                 && !r.isTerminal) with
            | false => rfl
            | true => exact absurd h h_any
          -- Old filter length = 0.
          have h_old_zero : (s.requests.filter p).length = 0 := by
            apply list_any_false_filter_length_zero
            simp only [hp_def]
            rw [← h_eq]
            exact h_any_false
          -- New request matches p because causedBy = some t.
          have h_pred_true : p { id := s!"dispatched-{s.requests.length}",
                                 causedBy := some (k, seed.causedByTriggerKind),
                                 concurrency := intent.concurrency,
                                 isTerminal := false,
                                 executionOrigin :=
                                   match seed.causedByTriggerKind with
                                   | .manual => .interactive
                                   | .schedule | .event => .scheduled } = true := by
            simp [hp_def, h_eq]
          show (List.filter p (s.requests ++ [_])).length ≤ 1
          rw [List.filter_append, List.filter_cons_of_pos (by rw [h_pred_true]),
              List.filter_nil, List.length_append, List.length_cons, List.length_nil,
              h_old_zero]
      · -- (k, seed.kind) ≠ t: new request's causedBy ≠ some t, count unchanged.
        by_cases h_any :
          s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                     && !r.isTerminal) = true
        · -- Skip branch: state unchanged.
          rw [if_pos h_any]
          exact h_before
        · -- Append branch: state is s.requests ++ [newRequest], newRequest.causedBy ≠ some t.
          rw [if_neg h_any]
          -- New request does not match p.
          have h_pred_false : p { id := s!"dispatched-{s.requests.length}",
                                  causedBy := some (k, seed.causedByTriggerKind),
                                  concurrency := intent.concurrency,
                                  isTerminal := false,
                                  executionOrigin :=
                                    match seed.causedByTriggerKind with
                                    | .manual => .interactive
                                    | .schedule | .event => .scheduled } = false := by
            simp only [hp_def]
            have h_ne : (k, seed.causedByTriggerKind) ≠ t := h_eq
            simp [h_ne]
          show (List.filter p (s.requests ++ [_])).length ≤ 1
          rw [List.filter_append, List.filter_cons_of_neg (by rw [h_pred_false]; decide),
              List.filter_nil, List.append_nil]
          exact h_before

/--
Post-step helper: if the post-state's requests for tuple `t` are all
`.serial`, and the dispatchStep's intent has `.parallel` concurrency,
then the post-state count for `t` is bounded by the pre-state count.

(Because a parallel new request with `causedBy = some t` would violate
the post-state hypothesis — so the new request either doesn't match `t`,
making the count unchanged, or the hypothesis forces a contradiction.)
-/
theorem dispatchStep_parallel_count_eq
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) (t : TriggerKey)
    (h_parallel : intent.concurrency = .parallel)
    (h_hyp_post : ∀ r ∈ (dispatchStep s snap intent).requests,
                  r.causedBy = some t → r.concurrency = .serial) :
    (dispatchStep s snap intent).nonTerminalCountFor t
      ≤ s.nonTerminalCountFor t := by
  -- Use dispatchStep_preserves_causedBy_and_concurrency indirectly:
  -- we'll prove the concrete membership facts.
  unfold SystemState.nonTerminalCountFor at *
  unfold dispatchStep at *
  cases h_disp : dispatch snap intent with
  | none =>
    simp only [h_disp]
    exact Nat.le_refl _
  | some seed =>
    simp only [h_disp, h_parallel] at *
    -- Goal and h_hyp_post are now about s.requests ++ [newRequest].
    set p : AgentRequest → Bool :=
      (fun r => (r.causedBy == some t) && !r.isTerminal) with hp_def
    -- Identify the new request
    set newRequest : AgentRequest :=
      { id := s!"dispatched-{s.requests.length}"
      , causedBy :=
          match seed.causedByTriggerId with
          | none     => none
          | some tid => some (tid, seed.causedByTriggerKind)
      , concurrency := .parallel
      , isTerminal := false
      , executionOrigin :=
          match seed.causedByTriggerKind with
          | .manual            => .interactive
          | .schedule | .event => .scheduled } with hnew_def
    by_cases h_match : p newRequest = true
    · -- Parallel new request matches t. But its concurrency is .parallel, not .serial.
      exfalso
      simp only [hp_def] at h_match
      have h_cb_eq : newRequest.causedBy = some t := by
        have := (Bool.and_eq_true _ _).mp h_match
        exact beq_iff_eq.mp this.1
      have h_mem_new : newRequest ∈ s.requests ++ [newRequest] :=
        List.mem_append_right _ (List.mem_singleton.mpr rfl)
      have h_serial := h_hyp_post newRequest h_mem_new h_cb_eq
      -- newRequest.concurrency = .parallel (by construction)
      have h_conc_par : newRequest.concurrency = .parallel := rfl
      rw [h_conc_par] at h_serial
      exact absurd h_serial (by decide)
    · have h_pred_false : p newRequest = false := by
        cases h : p newRequest with
        | false => rfl
        | true => exact absurd h h_match
      show (List.filter p (s.requests ++ [newRequest])).length ≤ (s.requests.filter p).length
      rw [List.filter_append, List.filter_cons_of_neg (by rw [h_pred_false]; decide),
          List.filter_nil, List.append_nil]

/--
Post-step helper: same as above but for `.latestOnly`. The `latestOnly`
operation can only decrease the count via supersession (flipping
`isTerminal := true`), and its new request — if it matched `t` — would
violate the hypothesis. So the post-state count is ≤ the pre-state count.
-/
theorem dispatchStep_latestOnly_count_le
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) (t : TriggerKey)
    (h_latest : intent.concurrency = .latestOnly)
    (h_hyp_post : ∀ r ∈ (dispatchStep s snap intent).requests,
                  r.causedBy = some t → r.concurrency = .serial) :
    (dispatchStep s snap intent).nonTerminalCountFor t
      ≤ s.nonTerminalCountFor t := by
  unfold SystemState.nonTerminalCountFor at *
  unfold dispatchStep at *
  cases h_disp : dispatch snap intent with
  | none =>
    simp only [h_disp]
    exact Nat.le_refl _
  | some seed =>
    simp only [h_disp, h_latest] at *
    set p : AgentRequest → Bool :=
      (fun r => (r.causedBy == some t) && !r.isTerminal) with hp_def
    cases h_key : seed.causedByTriggerId with
    | none =>
      simp only [h_key] at *
      set newRequest : AgentRequest :=
        { id := s!"dispatched-{s.requests.length}"
        , causedBy := none
        , concurrency := .latestOnly
        , isTerminal := false
        , executionOrigin :=
            match seed.causedByTriggerKind with
            | .manual            => .interactive
            | .schedule | .event => .scheduled } with hnew_def
      -- newRequest.causedBy = none, so p newRequest = false.
      have h_pred_false : p newRequest = false := by
        simp [hp_def, hnew_def]
      show (List.filter p (s.requests ++ [newRequest])).length ≤ (s.requests.filter p).length
      rw [List.filter_append, List.filter_cons_of_neg (by rw [h_pred_false]; decide),
          List.filter_nil, List.append_nil]
    | some tid =>
      simp only [h_key] at *
      set newRequest : AgentRequest :=
        { id := s!"dispatched-{s.requests.length}"
        , causedBy := some (tid, seed.causedByTriggerKind)
        , concurrency := .latestOnly
        , isTerminal := false
        , executionOrigin :=
            match seed.causedByTriggerKind with
            | .manual            => .interactive
            | .schedule | .event => .scheduled } with hnew_def
      set superseded : List AgentRequest :=
        s.requests.map (fun r =>
          if (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal then
            { r with isTerminal := true }
          else r) with hsup_def
      by_cases h_match : p newRequest = true
      · exfalso
        simp only [hp_def] at h_match
        have h_cb_eq : newRequest.causedBy = some t := by
          have := (Bool.and_eq_true _ _).mp h_match
          exact beq_iff_eq.mp this.1
        have h_mem_new : newRequest ∈ superseded ++ [newRequest] :=
          List.mem_append_right _ (List.mem_singleton.mpr rfl)
        have h_serial := h_hyp_post newRequest h_mem_new h_cb_eq
        have h_conc_lat : newRequest.concurrency = .latestOnly := rfl
        rw [h_conc_lat] at h_serial
        exact absurd h_serial (by decide)
      · have h_pred_false : p newRequest = false := by
          cases h : p newRequest with
          | false => rfl
          | true => exact absurd h h_match
        show (List.filter p (superseded ++ [newRequest])).length ≤ (s.requests.filter p).length
        rw [List.filter_append, List.filter_cons_of_neg (by rw [h_pred_false]; decide),
            List.filter_nil, List.append_nil]
        simp only [hsup_def]
        apply list_filter_map_length_le_filter_length
        intro r h_p_f_r
        by_cases h_cond : ((r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal) = true
        · rw [if_pos h_cond] at h_p_f_r
          simp [hp_def] at h_p_f_r
        · have h_cond_false : ((r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal) = false := by
            cases h : (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal with
            | false => rfl
            | true => exact absurd h h_cond
          rw [if_neg (by rw [h_cond_false]; decide)] at h_p_f_r
          exact h_p_f_r
