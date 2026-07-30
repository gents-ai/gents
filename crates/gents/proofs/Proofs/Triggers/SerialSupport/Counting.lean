import Proofs.Triggers.Reachability

private theorem list_any_false_filter_length_zero
    {α : Type} {p : α → Bool} {l : List α}
    (h : l.any p = false) : (l.filter p).length = 0 := by
  induction l with
  | nil => simp
  | cons hd tl ih =>
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    obtain ⟨h_hd, h_tl⟩ := h
    rw [List.filter_cons_of_neg (by rw [h_hd]; decide)]
    exact ih h_tl

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
    set p : AgentRequest → Bool :=
      (fun r => (r.causedBy == some t) && !r.isTerminal) with hp_def
    cases h_key : seed.causedByTriggerId with
    | none =>
      simp only
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
      simp only
      by_cases h_eq : (k, seed.causedByTriggerKind) = t
      ·
        by_cases h_any :
          s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                     && !r.isTerminal) = true
        ·
          rw [if_pos h_any]
          exact h_before
        ·
          rw [if_neg h_any]
          have h_any_false :
              s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                         && !r.isTerminal) = false := by
            cases h : s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                                 && !r.isTerminal) with
            | false => rfl
            | true => exact absurd h h_any
          have h_old_zero : (s.requests.filter p).length = 0 := by
            apply list_any_false_filter_length_zero
            simp only [hp_def]
            rw [← h_eq]
            exact h_any_false
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
      ·
        by_cases h_any :
          s.requests.any (fun r => (r.causedBy == some (k, seed.causedByTriggerKind))
                                     && !r.isTerminal) = true
        ·
          rw [if_pos h_any]
          exact h_before
        ·
          rw [if_neg h_any]
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

theorem dispatchStep_parallel_count_eq
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) (t : TriggerKey)
    (h_parallel : intent.concurrency = .parallel)
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
    simp only [h_disp, h_parallel] at *
    set p : AgentRequest → Bool :=
      (fun r => (r.causedBy == some t) && !r.isTerminal) with hp_def
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
    ·
      exfalso
      simp only [hp_def] at h_match
      have h_cb_eq : newRequest.causedBy = some t := by
        have := (Bool.and_eq_true _ _).mp h_match
        exact beq_iff_eq.mp this.1
      have h_mem_new : newRequest ∈ s.requests ++ [newRequest] :=
        List.mem_append_right _ (List.mem_singleton.mpr rfl)
      have h_serial := h_hyp_post newRequest h_mem_new h_cb_eq
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
