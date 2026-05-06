import Proofs.Triggers.Dispatch

/-!
# Trigger Reachability

Operational trigger steps, trace relations, and structural preservation lemmas.
-/

/--
One engine tick: given current state, snapshot, and a fire intent, produce
the next state.

Operational semantics match `trigger_engine/mod.rs`:
- Dispatch's enabled gate fails → state unchanged.
- Parallel → always append a new non-terminal request.
- Serial: skip if `key = some t` matches a non-terminal request; otherwise
  append. Serial with `key = none` (degenerate: Manual serial, unusual in
  practice) falls through to unconditional append — matches the Rust
  engine's "no trigger_id → no coordination" short-circuit.
- LatestOnly with `key = some t` → supersede all matching non-terminal
  (set `isTerminal = true`); then append the new request.
- LatestOnly with `key = none` → append unconditionally. Do NOT supersede
  other `causedBy = none` requests; they are unrelated manual fires.

Request IDs are derived from `state.requests.length` so every step produces
a fresh id. Preserves T3's `r_prior.id ≠ r_new.id` invariant.

Execution origin follows the spec's lineage map:
- manual → interactive
- schedule/event → scheduled
-/
def dispatchStep
    (state  : SystemState)
    (snap   : TriggerSnapshot)
    (intent : FireIntent)
    : SystemState :=
  match dispatch snap intent with
  | none => state
  | some seed =>
    let key : Option TriggerKey :=
      match seed.causedByTriggerId with
      | none     => none
      | some tid => some (tid, seed.causedByTriggerKind)
    let origin : ExecutionOrigin :=
      match seed.causedByTriggerKind with
      | .manual            => .interactive
      | .schedule | .event => .scheduled
    let newId : String := s!"dispatched-{state.requests.length}"
    let newRequest : AgentRequest :=
      { id := newId
      , causedBy := key
      , concurrency := intent.concurrency
      , isTerminal := false
      , executionOrigin := origin }
    match intent.concurrency with
    | .parallel =>
      { state with requests := state.requests ++ [newRequest] }
    | .serial =>
      match key with
      | none =>
        -- Degenerate serial with no trigger key: no coordination, append.
        { state with requests := state.requests ++ [newRequest] }
      | some t =>
        if state.requests.any (fun r => (r.causedBy == some t) && !r.isTerminal) then
          state  -- skip: non-terminal match exists
        else
          { state with requests := state.requests ++ [newRequest] }
    | .latestOnly =>
      match key with
      | none =>
        -- LatestOnly with no trigger key: matches Rust's "no trigger_id → skip
        -- lock + supersede" short-circuit at trigger_engine/mod.rs:343. Do NOT
        -- supersede other causedBy=none requests — they're unrelated.
        { state with requests := state.requests ++ [newRequest] }
      | some t =>
        let superseded := state.requests.map (fun r =>
          if (r.causedBy == some t) && !r.isTerminal then
            { r with isTerminal := true }
          else r)
        { state with requests := superseded ++ [newRequest] }

/--
Lifecycle terminal transition: flip a specified request's `isTerminal` from
`false` to `true`. Models the abstract "request completed/failed/superseded/
dead/interrupted" transition from `Request.lean::RequestState`.

We don't model the full lifecycle state machine — only the property that
any non-terminal request can transition to terminal. This is all T2 needs;
terminal transitions only decrease `nonTerminalCountFor`.

Does not create new requests, so has no id-collision risk with
`dispatchStep`'s `dispatched-N` naming scheme.
-/
def lifecycleTerminateStep (s : SystemState) (reqId : String) : SystemState :=
  { s with requests := s.requests.map (fun r =>
      if (r.id == reqId) && !r.isTerminal then
        { r with isTerminal := true }
      else r) }

/--
Reachable system states: built inductively from `SystemState.empty` via
- `step`: dispatch one fire intent against a snapshot.
- `terminate`: any non-terminal request can transition to terminal.

This is the raw operational semantics. It intentionally over-approximates the
input boundary by allowing any `FireIntent`; stronger spec-facing theorems can
prefer `ReachableUnder` / `WellFormedReachable` below.
-/
inductive Reachable : SystemState → Prop where
  | empty : Reachable SystemState.empty
  | step (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) :
      Reachable s → Reachable (dispatchStep s snap intent)
  | terminate (s : SystemState) (reqId : String) :
      Reachable s → Reachable (lifecycleTerminateStep s reqId)

/--
Strengthened trigger-engine reachability parameterized by an admissibility
predicate on fire intents.

This lets the proofs layer keep `Reachable` as the raw operational relation
while moving the real theorem surface onto a boundary-tightened trace relation.
The `terminate` constructor carries no extra boundary premise because it
consumes no new `FireIntent`; it only evolves an already-materialized request.
-/
inductive ReachableUnder (P : FireIntent → Prop) : SystemState → Prop where
  | empty : ReachableUnder P SystemState.empty
  | step (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) :
      P intent →
      ReachableUnder P s →
      ReachableUnder P (dispatchStep s snap intent)
  | terminate (s : SystemState) (reqId : String) :
      ReachableUnder P s →
      ReachableUnder P (lifecycleTerminateStep s reqId)

/-- Spec-facing strengthened reachability with the manual-intent boundary enforced. -/
abbrev WellFormedReachable : SystemState → Prop :=
  ReachableUnder FireIntent.WellFormed

/--
Spec-facing strengthened reachability where the trace boundary is both
well-formed for manual fires and serial for a distinguished trigger key `t`.
-/
abbrev SeriallyReachable (t : TriggerKey) : SystemState → Prop :=
  ReachableUnder (fun intent => intent.WellFormed ∧ intent.SerialForKey t)

/-- Any strengthened reachable state is reachable in the raw operational semantics. -/
theorem ReachableUnder.toReachable
    {P : FireIntent → Prop} {s : SystemState} :
    ReachableUnder P s → Reachable s := by
  intro h_reach
  induction h_reach with
  | empty =>
      exact Reachable.empty
  | step s snap intent _ h_prev ih =>
      exact Reachable.step s snap intent ih
  | terminate s reqId h_prev ih =>
      exact Reachable.terminate s reqId ih


/-- The empty state has zero non-terminal requests for any trigger tuple. -/
theorem nonTerminalCountFor_empty (t : TriggerKey) :
    SystemState.empty.nonTerminalCountFor t = 0 := by
  simp [SystemState.empty, SystemState.nonTerminalCountFor]

/--
Helper: any request in the pre-step state has a corresponding request in
the post-step state with the same `causedBy` and `concurrency` fields.
(The `isTerminal` field may flip under `.latestOnly` supersession.)

This is the core structural fact that `dispatchStep` preserves: it either
leaves requests alone, appends new ones, or flips `isTerminal`. It never
changes `causedBy` or `concurrency` on existing requests.
-/
private theorem dispatchStep_preserves_causedBy_and_concurrency
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent)
    (r : AgentRequest) :
    r ∈ s.requests →
    ∃ r' ∈ (dispatchStep s snap intent).requests,
      r'.causedBy = r.causedBy ∧ r'.concurrency = r.concurrency := by
  intro h_mem
  unfold dispatchStep
  -- Case on dispatch result
  cases h_disp : dispatch snap intent with
  | none =>
    -- state unchanged: r itself is the witness
    exact ⟨r, h_mem, rfl, rfl⟩
  | some seed =>
    simp only
    -- Case on concurrency
    cases h_conc : intent.concurrency with
    | parallel =>
      simp only
      refine ⟨r, ?_, rfl, rfl⟩
      exact List.mem_append_left _ h_mem
    | serial =>
      simp only
      cases h_key : seed.causedByTriggerId with
      | none =>
        simp only
        refine ⟨r, ?_, rfl, rfl⟩
        exact List.mem_append_left _ h_mem
      | some tid =>
        simp only
        by_cases h_any :
          s.requests.any (fun r => (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal) = true
        · rw [if_pos h_any]
          exact ⟨r, h_mem, rfl, rfl⟩
        · rw [if_neg h_any]
          refine ⟨r, ?_, rfl, rfl⟩
          exact List.mem_append_left _ h_mem
    | latestOnly =>
      simp only
      cases h_key : seed.causedByTriggerId with
      | none =>
        simp only
        refine ⟨r, ?_, rfl, rfl⟩
        exact List.mem_append_left _ h_mem
      | some tid =>
        simp only
        -- Post: (s.requests.map f) ++ [new] where f conditionally flips isTerminal.
        -- f preserves causedBy and concurrency regardless of branch.
        refine ⟨
          if (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal then
            { r with isTerminal := true }
          else r,
          ?_, ?_, ?_⟩
        · -- membership: f r ∈ (s.requests.map f) ++ [new]
          apply List.mem_append_left
          exact List.mem_map_of_mem _ h_mem
        · -- causedBy preserved
          split <;> rfl
        · -- concurrency preserved
          split <;> rfl


/--
If `dispatch` materializes a seed with a concrete trigger id, that id and
trigger kind came from the fire intent that dispatched it.
-/
private theorem dispatch_seed_some_triggerId_matches_intent
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed)
    {tid : String}
    (h_dispatch : dispatch snap intent = some seed)
    (h_seedId : seed.causedByTriggerId = some tid) :
    intent.triggerId = some tid ∧ intent.triggerKind = seed.causedByTriggerKind := by
  unfold dispatch at h_dispatch
  cases h_kind : intent.triggerKind with
  | schedule =>
    rw [h_kind] at h_dispatch
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some intentTid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForSchedule snap intentTid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        injection h_dispatch with h_seed_eq
        subst h_seed_eq
        simp at h_seedId
        obtain rfl := h_seedId
        exact ⟨rfl, rfl⟩
  | event =>
    rw [h_kind] at h_dispatch
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some intentTid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForEvent snap intentTid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        injection h_dispatch with h_seed_eq
        subst h_seed_eq
        simp at h_seedId
        obtain rfl := h_seedId
        exact ⟨rfl, rfl⟩
  | manual =>
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    injection h_dispatch with h_seed_eq
    subst h_seed_eq
    simp at h_seedId

/--
If a materialized seed carries trigger tuple `t`, then the dispatching intent
targeted `t` as well.
-/
theorem dispatch_key_matches_intent_target
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) (t : TriggerKey)
    (h_dispatch : dispatch snap intent = some seed)
    (h_key :
      (match seed.causedByTriggerId with
      | none => none
      | some tid => some (tid, seed.causedByTriggerKind)) = some t) :
    intent.triggerId = some t.1 ∧ intent.triggerKind = t.2 := by
  cases h_seedId : seed.causedByTriggerId with
  | none =>
    simp [h_seedId] at h_key
  | some tid =>
    have h_tuple : (tid, seed.causedByTriggerKind) = t := by
      simpa [h_seedId] using h_key
    have ⟨h_triggerId, h_kind⟩ :=
      dispatch_seed_some_triggerId_matches_intent snap intent seed h_dispatch h_seedId
    cases t with
    | mk tid' kind' =>
      cases h_tuple
      simpa using And.intro h_triggerId h_kind

/--
Post-hypothesis pre-state preservation for dispatchStep.

If the post-step state satisfies "every request for tuple `t` is serial",
then the pre-step state also satisfies it.

This is the bridge T2's induction uses to convert `h_hyp_post` into
`h_hyp_pre` for the inductive hypothesis `ih`.
-/
theorem dispatchStep_hypothesis_preservation
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) (t : TriggerKey)
    (h_hyp_post : ∀ r ∈ (dispatchStep s snap intent).requests,
                  r.causedBy = some t → r.concurrency = .serial) :
    ∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial := by
  intro r h_mem h_causedBy
  obtain ⟨r', h_mem', h_cb, h_conc⟩ :=
    dispatchStep_preserves_causedBy_and_concurrency s snap intent r h_mem
  have h_causedBy' : r'.causedBy = some t := h_cb.trans h_causedBy
  have h_serial' := h_hyp_post r' h_mem' h_causedBy'
  exact h_conc ▸ h_serial'

/--
Generic helper for mapped request lists whose update function preserves the
`causedBy` and `concurrency` fields.
-/
theorem map_member_has_preimage_preserving_causedBy_and_concurrency
    (s : SystemState)
    (f : AgentRequest → AgentRequest)
    (h_preserve : ∀ r, (f r).causedBy = r.causedBy ∧ (f r).concurrency = r.concurrency)
    (r : AgentRequest) :
    r ∈ s.requests.map f →
    ∃ r0 ∈ s.requests, r.causedBy = r0.causedBy ∧ r.concurrency = r0.concurrency := by
  intro h_mem
  rcases List.mem_map.mp h_mem with ⟨r0, h_mem0, h_eq⟩
  refine ⟨r0, h_mem0, ?_, ?_⟩
  · cases h_eq
    exact (h_preserve r0).1
  · cases h_eq
    exact (h_preserve r0).2


/--
Generic monotonicity helper: mapping a list with a function `f` that only
"weakens" the predicate `p` (i.e. `p (f a) = true → p a = true`) can only
shrink (or preserve) the length of the filtered list.

Used by `lifecycleTerminateStep_preserves_bound` below: the terminate map
only flips `isTerminal` from `false` to `true`, which can only remove
requests from the `(causedBy == some t) && !isTerminal` filter.
-/
theorem list_filter_map_length_le_filter_length
    {α : Type} {p : α → Bool} (f : α → α) (l : List α)
    (h_mono : ∀ a, p (f a) = true → p a = true) :
    ((l.map f).filter p).length ≤ (l.filter p).length := by
  induction l with
  | nil => simp
  | cons hd tl ih =>
    simp only [List.map_cons, List.filter_cons]
    by_cases h_p_f_hd : p (f hd) = true
    · -- p (f hd) = true, so p hd = true by h_mono.
      have h_p_hd : p hd = true := h_mono hd h_p_f_hd
      rw [if_pos h_p_f_hd, if_pos h_p_hd]
      simp only [List.length_cons]
      exact Nat.succ_le_succ ih
    · -- p (f hd) = false. Original filter may include hd or not.
      have h_p_f_hd_false : p (f hd) = false := by
        cases h : p (f hd) with
        | false => rfl
        | true => exact absurd h h_p_f_hd
      rw [if_neg (by rw [h_p_f_hd_false]; decide)]
      by_cases h_p_hd : p hd = true
      · rw [if_pos h_p_hd]
        simp only [List.length_cons]
        exact Nat.le_succ_of_le ih
      · have h_p_hd_false : p hd = false := by
          cases h : p hd with
          | false => rfl
          | true => exact absurd h h_p_hd
        rw [if_neg (by rw [h_p_hd_false]; decide)]
        exact ih

/--
`lifecycleTerminateStep` can only decrease (or leave unchanged) the
non-terminal count for any tuple. Flipping a request's `isTerminal` from
`false` to `true` removes it from the filter; no other request's
`isTerminal` or `causedBy` changes.
-/
theorem lifecycleTerminateStep_preserves_bound
    (s : SystemState) (reqId : String) (t : TriggerKey) :
    (lifecycleTerminateStep s reqId).nonTerminalCountFor t
      ≤ s.nonTerminalCountFor t := by
  simp only [SystemState.nonTerminalCountFor, lifecycleTerminateStep]
  apply list_filter_map_length_le_filter_length
  intro r h_p_f_r
  -- h_p_f_r : ((f r).causedBy == some t) && !(f r).isTerminal = true
  -- where f r = if (r.id == reqId) && !r.isTerminal
  --             then {r with isTerminal := true} else r
  cases h_cond : (r.id == reqId) && !r.isTerminal with
  | true =>
    -- if-fires branch: f r has isTerminal = true, so !isTerminal = false.
    -- Then the && in the predicate is false, contradicting h_p_f_r.
    rw [if_pos h_cond] at h_p_f_r
    -- h_p_f_r : (({r with isTerminal := true}.causedBy == some t) &&
    --           !({r with isTerminal := true}.isTerminal)) = true
    -- But {r with isTerminal := true}.isTerminal = true, so !...=false, so && = false.
    simp at h_p_f_r
  | false =>
    -- if-doesn't-fire branch: f r = r.
    rw [if_neg (by rw [h_cond]; decide)] at h_p_f_r
    exact h_p_f_r

/--
Helper: `lifecycleTerminateStep` preserves `causedBy` and `concurrency`
on existing requests. Any request in the pre-state has a corresponding
request in the post-state with the same two fields — the only possible
mutation is flipping `isTerminal := true`.
-/
theorem lifecycleTerminateStep_preserves_causedBy_and_concurrency
    (s : SystemState) (reqId : String) (r : AgentRequest) :
    r ∈ s.requests →
    ∃ r' ∈ (lifecycleTerminateStep s reqId).requests,
      r'.causedBy = r.causedBy ∧ r'.concurrency = r.concurrency := by
  intro h_mem
  unfold lifecycleTerminateStep
  simp only
  refine ⟨_, List.mem_map_of_mem _ h_mem, ?_, ?_⟩
  · split <;> rfl
  · split <;> rfl
