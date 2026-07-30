import Proofs.Triggers.Reachability

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
