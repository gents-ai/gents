import Proofs.Triggers.Reachability

theorem T3_latest_only_convergence
    (before : SystemState) (snap : TriggerSnapshot) (intent : FireIntent)
    (seed : RequestSeed) (t : TriggerKey)
    (h_dispatch : dispatch snap intent = some seed)
    (h_latest : intent.concurrency = .latestOnly)
    (h_key :
      (match seed.causedByTriggerId with
       | none => none
       | some tid => some tid) = some t) :
    ∀ r_prior ∈ before.requests,
      r_prior.causedBy = some t ∧ r_prior.isTerminal = false →
      ∃ r_prior_after ∈ (dispatchStep before snap intent).requests,
        r_prior_after.id = r_prior.id ∧ r_prior_after.isTerminal = true := by
  intro r_prior h_mem h_cond
  cases h_seedId : seed.causedByTriggerId with
  | none =>
      simp [h_seedId] at h_key
  | some tid =>
      have h_id : tid = t := by
        simpa [h_seedId] using h_key
      have h_cb : r_prior.causedBy = some tid := by
        rw [h_cond.1, h_id]
      have h_mapped :
          { r_prior with isTerminal := true } ∈
            before.requests.map (fun r =>
              if (r.causedBy == some tid) && !r.isTerminal then
                { r with isTerminal := true }
              else r) := by
        have h_update :
            (fun r =>
              if (r.causedBy == some tid) && !r.isTerminal then
                { r with isTerminal := true }
              else r) r_prior = { r_prior with isTerminal := true } := by
          simp [h_cb, h_cond.2]
        rw [← h_update]
        exact List.mem_map_of_mem _ h_mem
      refine ⟨{ r_prior with isTerminal := true }, ?_, by simp, by simp⟩
      have h_after :=
        List.mem_append_left
          [{ id := s!"dispatched-{before.requests.length}",
             causedBy := some tid,
             concurrency := .latestOnly,
             isTerminal := false,
             executionOrigin :=
               match seed.causedByTriggerKind with
               | .manual => .interactive
               | .schedule | .event => .scheduled }]
          h_mapped
      simpa [dispatchStep, h_dispatch, h_latest, h_seedId] using h_after
