import Proofs.Triggers.Reachability

/-!
# Latest-Only Trigger Theorems

Supersession semantics for latest-only trigger fires.
-/

/-- Terminal predicate matching the `superseded` state. Kept as a Bool
    field on `AgentRequest` so the trigger layer can reason without
    unfolding the full `RequestState`. -/
def AgentRequest.isSuperseded (r : AgentRequest) : Prop :=
  r.isTerminal = true

/-- Abstract relation modeling a `latestOnly` fire that atomically
    materializes `r_new` into the system state and supersedes all prior
    non-terminal requests for the same trigger key. -/
def latestOnlyFireTransition
    (before after : SystemState) (t : TriggerKey) (r_new : AgentRequest) : Prop :=
  r_new.causedBy = some t ∧
  r_new.concurrency = .latestOnly ∧
  r_new.isTerminal = false ∧
  r_new ∈ after.requests ∧
  -- All prior non-terminal requests for `t` are present in `after` with
  -- `isTerminal = true` (i.e. superseded).
  (∀ r_prior ∈ before.requests,
    r_prior.causedBy = some t ∧ r_prior.isTerminal = false ∧ r_prior.id ≠ r_new.id →
    ∃ r_prior_after ∈ after.requests,
      r_prior_after.id = r_prior.id ∧ r_prior_after.isTerminal = true) ∧
  -- Requests for other triggers are untouched.
  (∀ r ∈ before.requests, r.causedBy ≠ some t →
    r ∈ after.requests)

/-- Abstract latest-only convergence lemma.

This only unwraps `latestOnlyFireTransition`; the public T3 theorem below proves
the same supersession fact directly from `dispatchStep`. -/
theorem latestOnlyFireTransition_convergence
    (before after : SystemState) (t : TriggerKey) (r_new : AgentRequest) :
    latestOnlyFireTransition before after t r_new →
    ∀ r_prior ∈ before.requests,
      r_prior.causedBy = some t ∧ r_prior.isTerminal = false ∧
        r_prior.id ≠ r_new.id →
      ∃ r_prior_after ∈ after.requests,
        r_prior_after.id = r_prior.id ∧ r_prior_after.isTerminal = true := by
  intro h_trans r_prior h_mem h_cond
  rcases h_trans with ⟨_, _, _, _, h_super, _⟩
  exact h_super r_prior h_mem h_cond

/-- **Theorem T3 (latest_only convergence, executable dispatch form).**

If `dispatchStep` executes a successful `.latestOnly` fire for trigger key `t`,
then every prior non-terminal request with `causedBy = some t` is present in the
post-step state with `isTerminal = true`.

This is the behavior of the concrete executable dispatcher, not just the
abstract `latestOnlyFireTransition` relation above. -/
theorem T3_latest_only_convergence
    (before : SystemState) (snap : TriggerSnapshot) (intent : FireIntent)
    (seed : RequestSeed) (t : TriggerKey)
    (h_dispatch : dispatch snap intent = some seed)
    (h_latest : intent.concurrency = .latestOnly)
    (h_key :
      (match seed.causedByTriggerId with
       | none => none
       | some tid => some (tid, seed.causedByTriggerKind)) = some t) :
    ∀ r_prior ∈ before.requests,
      r_prior.causedBy = some t ∧ r_prior.isTerminal = false →
      ∃ r_prior_after ∈ (dispatchStep before snap intent).requests,
        r_prior_after.id = r_prior.id ∧ r_prior_after.isTerminal = true := by
  intro r_prior h_mem h_cond
  cases h_seedId : seed.causedByTriggerId with
  | none =>
      simp [h_seedId] at h_key
  | some tid =>
      have h_tuple : (tid, seed.causedByTriggerKind) = t := by
        simpa [h_seedId] using h_key
      have h_cb : r_prior.causedBy = some (tid, seed.causedByTriggerKind) := by
        rw [h_cond.1, h_tuple]
      have h_mapped :
          { r_prior with isTerminal := true } ∈
            before.requests.map (fun r =>
              if (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal then
                { r with isTerminal := true }
              else r) := by
        have h_update :
            (fun r =>
              if (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal then
                { r with isTerminal := true }
              else r) r_prior = { r_prior with isTerminal := true } := by
          simp [h_cb, h_cond.2]
        rw [← h_update]
        exact List.mem_map_of_mem _ h_mem
      refine ⟨{ r_prior with isTerminal := true }, ?_, by simp, by simp⟩
      have h_after :=
        List.mem_append_left
          [{ id := s!"dispatched-{before.requests.length}",
             causedBy := some (tid, seed.causedByTriggerKind),
             concurrency := .latestOnly,
             isTerminal := false,
             executionOrigin :=
               match seed.causedByTriggerKind with
               | .manual => .interactive
               | .schedule | .event => .scheduled }]
          h_mapped
      simpa [dispatchStep, h_dispatch, h_latest, h_seedId] using h_after
