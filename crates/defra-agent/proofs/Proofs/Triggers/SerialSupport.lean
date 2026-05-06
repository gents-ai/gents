import Proofs.Triggers.Reachability

/-!
# Serial Trigger Support

Counting and preservation helpers used by the public serial-trigger theorems.
-/

/--
Trace-boundary preservation for the current T2 seriality hypothesis.

If every pre-state request for tuple `t` is serial, and the incoming intent is
serial whenever it targets `t`, then every post-state request for `t` is serial
after one `dispatchStep`.
-/
private theorem dispatchStep_preserves_target_seriality
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) (t : TriggerKey)
    (h_before : ∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial)
    (h_serial : intent.SerialForKey t) :
    ∀ r ∈ (dispatchStep s snap intent).requests,
      r.causedBy = some t →
      r.concurrency = .serial := by
  intro r h_mem h_causedBy
  unfold dispatchStep at h_mem
  cases h_disp : dispatch snap intent with
  | none =>
    rw [h_disp] at h_mem
    exact h_before r h_mem h_causedBy
  | some seed =>
    cases h_conc : intent.concurrency with
    | parallel =>
      rw [h_disp, h_conc] at h_mem
      rcases List.mem_append.mp h_mem with h_old | h_new
      · exact h_before r h_old h_causedBy
      · have h_new_req :
          r =
            { id := s!"dispatched-{s.requests.length}"
            , causedBy :=
                match seed.causedByTriggerId with
                | none => none
                | some tid => some (tid, seed.causedByTriggerKind)
            , concurrency := .parallel
            , isTerminal := false
            , executionOrigin :=
                match seed.causedByTriggerKind with
                | .manual => .interactive
                | .schedule | .event => .scheduled } := by
          simpa using h_new
        cases h_new_req
        have ⟨h_triggerId, h_kind⟩ :=
          dispatch_key_matches_intent_target snap intent seed t h_disp h_causedBy
        have h_intent_serial :=
          FireIntent.serialForKey_target_is_serial h_serial h_triggerId h_kind
        rw [h_conc] at h_intent_serial
        cases h_intent_serial
    | serial =>
      cases h_key : seed.causedByTriggerId with
      | none =>
        simp [h_disp, h_conc, h_key] at h_mem
        rcases h_mem with h_old | h_new
        · exact h_before r h_old h_causedBy
        · cases h_new
          simp at h_causedBy
      | some tid =>
        simp [h_disp, h_conc, h_key] at h_mem
        by_cases h_any :
            ∃ x ∈ s.requests, x.causedBy = some (tid, seed.causedByTriggerKind) ∧ x.isTerminal = false
        · rw [if_pos h_any] at h_mem
          exact h_before r h_mem h_causedBy
        · rw [if_neg h_any] at h_mem
          rcases List.mem_append.mp h_mem with h_old | h_new
          · exact h_before r h_old h_causedBy
          · have h_new_req :
              r =
                { id := s!"dispatched-{s.requests.length}"
                , causedBy := some (tid, seed.causedByTriggerKind)
                , concurrency := .serial
                , isTerminal := false
                , executionOrigin :=
                    match seed.causedByTriggerKind with
                    | .manual => .interactive
                    | .schedule | .event => .scheduled } := by
              simpa using h_new
            cases h_new_req
            simp
    | latestOnly =>
      cases h_key : seed.causedByTriggerId with
      | none =>
        simp [h_disp, h_conc, h_key] at h_mem
        rcases h_mem with h_old | h_new
        · exact h_before r h_old h_causedBy
        · cases h_new
          simp at h_causedBy
      | some tid =>
        simp [h_disp, h_conc, h_key] at h_mem
        rcases h_mem with h_superseded | h_new
        ·
          have h_map_mem :
              r ∈
                s.requests.map (fun r =>
                  if r.causedBy = some (tid, seed.causedByTriggerKind) ∧ r.isTerminal = false then
                    { r with isTerminal := true }
                  else r) := by
            simpa using h_superseded
          obtain ⟨r0, h_mem0, h_cb, h_conc'⟩ :=
            map_member_has_preimage_preserving_causedBy_and_concurrency s
              (fun r =>
                if r.causedBy = some (tid, seed.causedByTriggerKind) ∧ r.isTerminal = false then
                  { r with isTerminal := true }
                else r)
              (by
                intro r0
                by_cases h_cond :
                    r0.causedBy = some (tid, seed.causedByTriggerKind) ∧ r0.isTerminal = false <;>
                  simp [h_cond])
              r
              h_map_mem
          have h_cb' : r0.causedBy = some t := by
            rw [← h_cb]
            exact h_causedBy
          have h_serial0 := h_before r0 h_mem0 h_cb'
          rw [h_conc']
          exact h_serial0
        · have h_new_req :
            r =
              { id := s!"dispatched-{s.requests.length}"
              , causedBy := some (tid, seed.causedByTriggerKind)
              , concurrency := .latestOnly
              , isTerminal := false
              , executionOrigin :=
                  match seed.causedByTriggerKind with
                  | .manual => .interactive
                  | .schedule | .event => .scheduled } := by
            simpa using h_new
          cases h_new_req
          have h_key_match :
              (match seed.causedByTriggerId with
              | none => none
              | some tid' => some (tid', seed.causedByTriggerKind)) = some t := by
            simpa [h_key] using h_causedBy
          have ⟨h_triggerId, h_kind⟩ :=
            dispatch_key_matches_intent_target snap intent seed t h_disp h_key_match
          have h_intent_serial :=
            FireIntent.serialForKey_target_is_serial h_serial h_triggerId h_kind
          rw [h_conc] at h_intent_serial
          cases h_intent_serial


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


/--
Lifecycle terminal transitions preserve the seriality hypothesis for requests
already keyed to `t`, because they do not alter `causedBy` or `concurrency`.
-/
theorem lifecycleTerminateStep_preserves_target_seriality
    (s : SystemState) (reqId : String) (t : TriggerKey)
    (h_before : ∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial) :
    ∀ r ∈ (lifecycleTerminateStep s reqId).requests,
      r.causedBy = some t →
      r.concurrency = .serial := by
  intro r h_mem h_causedBy
  obtain ⟨r0, h_mem0, h_cb, h_conc⟩ :=
    map_member_has_preimage_preserving_causedBy_and_concurrency s
      (fun r =>
        if (r.id == reqId) && !r.isTerminal then
          { r with isTerminal := true }
        else r)
      (by
        intro r0
        by_cases h_cond : ((r0.id == reqId) && !r0.isTerminal) = true <;>
          simp [h_cond])
      r
      (by simpa [lifecycleTerminateStep] using h_mem)
  have h_cb' : r0.causedBy = some t := by
    rw [← h_cb]
    exact h_causedBy
  have h_serial0 := h_before r0 h_mem0 h_cb'
  rw [h_conc]
  exact h_serial0

/--
Any request for tuple `t` in a `SeriallyReachable t` state is serial.

This is the pre-trace bridge from the strengthened trace boundary back to T2's
original post-state hypothesis shape.
-/
theorem seriallyReachable_requests_for_key_are_serial
    (s : SystemState) (t : TriggerKey)
    (h_reach : SeriallyReachable t s) :
    ∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial := by
  induction h_reach with
  | empty =>
    intro r h_mem h_causedBy
    simp [SystemState.empty] at h_mem
  | step s snap intent h_boundary h_prev ih =>
    exact dispatchStep_preserves_target_seriality s snap intent t ih h_boundary.2
  | terminate s reqId h_prev ih =>
    exact lifecycleTerminateStep_preserves_target_seriality s reqId t ih
