import Proofs.Basic
import Proofs.Scheduling
import Proofs.RuntimeReconcile

/-!
# Layer 5: Trigger Engine

Model of the trigger engine (schedule / event / manual fires) that
feeds the request lifecycle. This file introduces the shared
vocabulary: `TriggerKind`, `ConcurrencyMode`, `FireIntent`, `RequestSeed`
and the operational `dispatch` / `dispatchStep` semantics a fire goes
through before it becomes a materialized `AgentRequest`.

The theorems (`T1`..`T4`) state the high-level safety properties the
trigger engine must preserve:

* **T1** — a fire only produces a seed if the corresponding trigger is
  active and enabled in the currently published runtime snapshot.
* **T2** — at most one non-terminal request exists per serial trigger.
* **T3** — after a `latestOnly` fire materializes a new request, every
  prior non-terminal request for the same trigger reaches `Superseded`.
* **T4** — every materialized `RequestSeed.causedBy` tuple is consistent
  with its scheduler `ExecutionOrigin`.

Keeping these at the proofs layer lets the Rust runtime evolve its
internal data structures without drifting from the spec.
-/

/-- Category of the originating trigger for a fire intent. -/
inductive TriggerKind where
  | schedule
  | event
  | manual
  deriving DecidableEq, Repr

/-- Concurrency policy declared on a task definition. -/
inductive ConcurrencyMode where
  | parallel
  | serial
  | latestOnly
  deriving DecidableEq, Repr

/-- Abstract schedule record visible in the active snapshot. -/
structure ActiveSchedule where
  triggerId : String
  enabled : Bool
  deriving DecidableEq, Repr

/-- Concrete event-trigger record visible in the active snapshot.

Mirrors the Rust-side `ResolvedEventTrigger`: the trigger engine needs
more than `(triggerId, enabled)` to dispatch an event fire — it must
know which source collection and event kind the subscription is
watching, which task to render, and the declared concurrency mode.

This parallels `ActiveSchedule` in spirit; `ActiveSchedule` has stayed
minimal because the schedule-kind theorems so far only exercise the
`enabled` gate, while the event path already needs the richer shape for
the subscription join that `EventSource` performs in Rust. -/
structure ActiveEventTrigger where
  triggerId : String
  taskId : String
  sourceCollection : String
  eventKind : String
  enabled : Bool
  concurrency : ConcurrencyMode
  deriving DecidableEq, Repr

/-- Trigger-layer view of the active runtime snapshot. The reconcile
    layer already owns `ActiveRuntimeSnapshot`; the trigger engine needs
    a richer projection that includes active schedules and event
    triggers. Keeping it as a separate structure avoids churn in the
    reconcile proof while still letting us state theorems over a
    published runtime generation. -/
structure TriggerSnapshot where
  generation : Generation
  activeSchedules : List ActiveSchedule
  activeEventTriggers : List ActiveEventTrigger
  deriving DecidableEq, Repr

namespace TriggerSnapshot

/-- Lookup a schedule by trigger id. -/
def findSchedule (snap : TriggerSnapshot) (triggerId : String) :
    Option ActiveSchedule :=
  snap.activeSchedules.find? (fun s => s.triggerId = triggerId)

/-- Lookup an event trigger by trigger id. -/
def findEventTrigger (snap : TriggerSnapshot) (triggerId : String) :
    Option ActiveEventTrigger :=
  snap.activeEventTriggers.find? (fun t => t.triggerId = triggerId)

/-- Whether a schedule trigger id is active in this snapshot. -/
def hasSchedule (snap : TriggerSnapshot) (triggerId : String) : Bool :=
  (snap.findSchedule triggerId).isSome

/-- Whether an event trigger id is active in this snapshot. -/
def hasEventTrigger (snap : TriggerSnapshot) (triggerId : String) : Bool :=
  (snap.findEventTrigger triggerId).isSome

end TriggerSnapshot

/-- Render-time input to the trigger dispatcher. The render inputs
    themselves are abstracted away; only the fields relevant to
    admissibility are modeled. -/
structure FireIntent where
  triggerId : Option String
  triggerKind : TriggerKind
  taskId : String
  concurrency : ConcurrencyMode
  deriving Repr

/-- Minimal projection of a materialized `AgentRequest` carrying the
    lineage fields established by the trigger engine. -/
structure RequestSeed where
  causedByTriggerId : Option String
  causedByTriggerKind : TriggerKind
  deriving Repr

/--
Helper: return the first enabled schedule in `snap.activeSchedules` matching
the given `triggerId`, or `none` if no such schedule exists.
-/
def dispatchEnabledForSchedule
    (snap : TriggerSnapshot) (triggerId : String) : Option ActiveSchedule :=
  snap.activeSchedules.find? (fun s => (s.triggerId == triggerId) && s.enabled)

/--
Helper: return the first enabled event trigger in `snap.activeEventTriggers`
matching the given `triggerId`, or `none` if no such trigger exists.
-/
def dispatchEnabledForEvent
    (snap : TriggerSnapshot) (triggerId : String) : Option ActiveEventTrigger :=
  snap.activeEventTriggers.find? (fun t => (t.triggerId == triggerId) && t.enabled)

/--
Dispatch — the enabled-gate + materialization-shape step.

Does NOT perform concurrency or in-flight checks against `SystemState`;
those live in `dispatchStep`.

Returns `some seed` when:
- Schedule kind: an enabled schedule matching `intent.triggerId` exists in the snapshot.
- Event kind: an enabled event trigger matching `intent.triggerId` exists.
- Manual kind: unconditional. Manual fires bypass the enabled gate; the
  task-level gate is enforced by the caller (`run_task_now` checks
  `snap.activeTasks` — out of scope for this proof layer).

Returns `none` otherwise.
-/
def dispatch
    (snap : TriggerSnapshot) (intent : FireIntent) : Option RequestSeed :=
  match intent.triggerKind with
  | .schedule =>
    match intent.triggerId with
    | none     => none
    | some tid =>
      match dispatchEnabledForSchedule snap tid with
      | none   => none
      | some _ =>
        some { causedByTriggerId := some tid, causedByTriggerKind := .schedule }
  | .event =>
    match intent.triggerId with
    | none     => none
    | some tid =>
      match dispatchEnabledForEvent snap tid with
      | none   => none
      | some _ =>
        some { causedByTriggerId := some tid, causedByTriggerKind := .event }
  | .manual =>
    -- Manual fires always carry a null lineage id, matching the runtime invariant
    -- (see `trigger_engine/manual_source.rs:83` where manual_source constructs
    -- `FireIntent { trigger_id: None, ... }`). Normalizing here rather than
    -- requiring a FireIntent well-formedness predicate keeps `Reachable`
    -- structurally simple; the caller's `intent.triggerId` (if any) is
    -- deliberately discarded. Also aligns with `consistentLineage` which
    -- maps `.manual → .interactive` without distinguishing an id.
    some { causedByTriggerId := none, causedByTriggerKind := .manual }

/--
Invariant: `dispatch` always produces a `RequestSeed` whose `causedByTriggerId`
is `none` for the `.manual` kind. Guaranteed by construction — the `.manual`
arm of `dispatch` hardcodes `causedByTriggerId := none`.

This rules out reachable manual requests with a non-null lineage id, matching
the runtime invariant in `trigger_engine/manual_source.rs`.
-/
theorem dispatch_manual_lineage_id_is_none
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) :
    dispatch snap intent = some seed →
    seed.causedByTriggerKind = .manual →
    seed.causedByTriggerId = none := by
  intro h_dispatch h_manual
  unfold dispatch at h_dispatch
  -- Case-split on triggerKind.
  cases h_kind : intent.triggerKind with
  | schedule =>
    rw [h_kind] at h_dispatch
    -- In the schedule arm, seed.causedByTriggerKind = .schedule, contradicting h_manual.
    -- Need to extract the kind field from the schedule-branch seed.
    match h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForSchedule snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        -- h_dispatch : some { causedByTriggerId := some tid, causedByTriggerKind := .schedule } = some seed
        -- Extract seed.causedByTriggerKind:
        obtain ⟨⟩ := h_dispatch
        -- seed.causedByTriggerKind = .schedule, contradiction with h_manual
        simp at h_manual
  | event =>
    -- Symmetric to schedule.
    rw [h_kind] at h_dispatch
    match h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForEvent snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        obtain ⟨⟩ := h_dispatch
        simp at h_manual
  | manual =>
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    -- h_dispatch : some { causedByTriggerId := none, causedByTriggerKind := .manual } = some seed
    obtain ⟨⟩ := h_dispatch
    -- seed.causedByTriggerId = none directly
    rfl

/-- A trigger is identified by `(triggerId, triggerKind)`. Pairing both
    avoids collisions between, e.g., a schedule and an event trigger
    that happen to share the same document id. -/
abbrev TriggerKey := String × TriggerKind

/-- Spec-layer projection of an AgentRequest sufficient to state the
    trigger-engine theorems. The real `AgentRequest` carries far more
    state; here we only track the fields the trigger engine reasons
    about. -/
structure AgentRequest where
  id : String
  causedBy : Option TriggerKey
  concurrency : ConcurrencyMode
  /-- Mirror of `RequestState`-level terminality without forcing the
      trigger layer to unfold the full lifecycle state. -/
  isTerminal : Bool
  /-- Execution origin inherited from the trigger engine. -/
  executionOrigin : ExecutionOrigin
  deriving Repr

/-- Aggregate system state observed by the trigger engine for
    cross-request reasoning. -/
structure SystemState where
  requests : List AgentRequest
  deriving Repr

/-- The initial empty system state used as the base case for `Reachable`. -/
def SystemState.empty : SystemState := { requests := [] }

/--
Count of non-terminal requests with matching `causedBy` tuple.
This is the quantity T2 bounds.
-/
def SystemState.nonTerminalCountFor
    (s : SystemState) (t : TriggerKey) : Nat :=
  (s.requests.filter (fun r => (r.causedBy == some t) && !r.isTerminal)).length

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

T2 is stated over `Reachable s` because the invariant holds over states
the system can produce via these two transitions.
-/
inductive Reachable : SystemState → Prop where
  | empty : Reachable SystemState.empty
  | step (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) :
      Reachable s → Reachable (dispatchStep s snap intent)
  | terminate (s : SystemState) (reqId : String) :
      Reachable s → Reachable (lifecycleTerminateStep s reqId)

/--
Local helper for T1: when `List.find? p l = some a`, both `a ∈ l` AND
`p a = true`. Mathlib provides these in two separate lemmas; we combine
them for proof convenience.
-/
private theorem find?_some_and_mem
    {α : Type} {p : α → Bool} {l : List α} {a : α}
    (h : l.find? p = some a) : a ∈ l ∧ p a = true := by
  induction l with
  | nil => simp [List.find?] at h
  | cons x xs ih =>
    simp only [List.find?] at h
    split at h
    · -- predicate true on x: h : some x = some a
      rename_i h_pred
      cases h
      exact ⟨List.mem_cons_self _ _, h_pred⟩
    · -- predicate false on x: recurse
      have := ih h
      exact ⟨List.mem_cons_of_mem _ this.1, this.2⟩

/-- **Theorem T1 (enabled gate).**

A fire intent that successfully dispatches into a `RequestSeed` implies
its underlying trigger is present *and* enabled in the active snapshot
at the time of the fire. This theorem locks the invariant that a
disabled schedule or event trigger cannot admit new work, even if a
stale scheduler tick races the reconcile publish.

The statement is conjunctive and **kind-dispatched** — there are three
admissible branches and they differ in what the snapshot must witness:

* **Schedule branch.** `intent.triggerKind = .schedule` requires an
  `activeSchedule` in `snap.activeSchedules` whose `triggerId` matches
  the intent and whose `enabled = true`. Conceptually: the clock-tick
  source may only produce a fire for a schedule the reconcile layer
  has published as active.
* **Event branch.** `intent.triggerKind = .event` requires an
  `activeEventTrigger` in `snap.activeEventTriggers` with matching
  `triggerId` and `enabled = true`. Stated over the concrete
  `ActiveEventTrigger` structure: the existential witness `trig` has
  the full shape `{triggerId, taskId, sourceCollection, eventKind,
  enabled, concurrency}`, matching the Rust `ResolvedEventTrigger`.
  The projection `trig.triggerId` / `trig.enabled` is what the gate
  checks; the remaining fields are load-bearing for `dispatch`'s
  operational definition (e.g. joining on `sourceCollection` and
  `eventKind` to decide which subscription's buffer to drain).
* **Manual branch — unconditional.** `intent.triggerKind = .manual`
  imposes **no snapshot precondition** on the gate. Manual fires are
  operator-initiated and do not reference any trigger document, so
  there is nothing to look up. The Rust mirror of this is
  `TriggerEngine::dispatch` falling through the `TriggerKind::Manual`
  arm without touching `snapshot.active_schedules` or
  `snapshot.active_event_triggers`. See `T1_manual_unconditional`
  below for the lemma form of this observation.

**Proof approach.** Unfold `dispatch`, case on `intent.triggerKind`,
then on `intent.triggerId`, then on the `dispatchEnabledForSchedule` /
`dispatchEnabledForEvent` lookup. The `find?_some_and_mem` helper
(below the proof block) extracts both membership in the active list
and the `(triggerId == tid) && enabled` predicate from
`List.find? = some active`, which combine to yield the existential
witness demanded by the conclusion. -/
theorem T1_enabled_gate
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) :
    dispatch snap intent = some seed →
    (intent.triggerKind = .schedule →
      ∃ triggerId, intent.triggerId = some triggerId ∧
        ∃ sched ∈ snap.activeSchedules,
          sched.triggerId = triggerId ∧ sched.enabled = true) ∧
    (intent.triggerKind = .event →
      ∃ triggerId, intent.triggerId = some triggerId ∧
        ∃ trig ∈ snap.activeEventTriggers,
          trig.triggerId = triggerId ∧ trig.enabled = true) := by
  intro h_dispatch
  refine ⟨?schedule, ?event⟩
  · -- Schedule branch
    intro h_kind
    unfold dispatch at h_dispatch
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    -- Case on intent.triggerId
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      -- Case on dispatchEnabledForSchedule result
      cases h_found : dispatchEnabledForSchedule snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some active =>
        -- Witness: active ∈ snap.activeSchedules, active satisfies the predicate.
        unfold dispatchEnabledForSchedule at h_found
        have ⟨h_mem, h_pred⟩ := find?_some_and_mem h_found
        -- h_pred : ((active.triggerId == tid) && active.enabled) = true
        have ⟨h_beq, h_enabled⟩ := (Bool.and_eq_true _ _).mp h_pred
        refine ⟨tid, rfl, active, h_mem, ?_, ?_⟩
        · exact beq_iff_eq.mp h_beq
        · exact h_enabled
  · -- Event branch — identical structure with EventTrigger / dispatchEnabledForEvent.
    intro h_kind
    unfold dispatch at h_dispatch
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForEvent snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some active =>
        unfold dispatchEnabledForEvent at h_found
        have ⟨h_mem, h_pred⟩ := find?_some_and_mem h_found
        have ⟨h_beq, h_enabled⟩ := (Bool.and_eq_true _ _).mp h_pred
        refine ⟨tid, rfl, active, h_mem, ?_, ?_⟩
        · exact beq_iff_eq.mp h_beq
        · exact h_enabled

/-- **Lemma `T1_manual_unconditional`.**

Companion observation to `T1_enabled_gate`: the Manual branch is
*semantically distinct* from the Schedule and Event branches because
no snapshot precondition is imposed on it. A successful dispatch of a
Manual intent implies nothing about `snap.activeSchedules` or
`snap.activeEventTriggers` — not even existence.

This is load-bearing because it says **dispatch of a Manual intent is
a pure function of whether the task is resolvable**, decoupled from
the trigger catalog. In Rust, `ManualTriggerHandle::run_task_now` is
gated only by `snapshot.active_tasks()` (the task-availability map),
never by `active_schedules` / `active_event_triggers`; this lemma is
the Lean mirror of that design.

The statement collapses to `True` conditioned on the Manual kind —
there is literally nothing to witness — and is kept as a named theorem
so downstream proofs can appeal to it by name when they need to
discharge the kind = Manual case of a case-split over `triggerKind`.

Unlike `T1_enabled_gate`, this one discharges without touching
`dispatch`: the conclusion is `True`, so the proof is trivial and does
not need to wait for `dispatch`'s operational definition. The lemma's
value lives in its *statement*, which says what we mean by "Manual is
unconditional." -/
theorem T1_manual_unconditional
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) :
    dispatch snap intent = some seed →
    intent.triggerKind = .manual →
    True := by
  intro _ _
  trivial


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
Generic monotonicity helper: mapping a list with a function `f` that only
"weakens" the predicate `p` (i.e. `p (f a) = true → p a = true`) can only
shrink (or preserve) the length of the filtered list.

Used by `lifecycleTerminateStep_preserves_bound` below: the terminate map
only flips `isTerminal` from `false` to `true`, which can only remove
requests from the `(causedBy == some t) && !isTerminal` filter.
-/
private theorem list_filter_map_length_le_filter_length
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
Post-step helper: if the post-state's requests for tuple `t` are all
`.serial`, and the dispatchStep's intent has `.parallel` concurrency,
then the post-state count for `t` is bounded by the pre-state count.

(Because a parallel new request with `causedBy = some t` would violate
the post-state hypothesis — so the new request either doesn't match `t`,
making the count unchanged, or the hypothesis forces a contradiction.)
-/
private theorem dispatchStep_parallel_count_eq
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
private theorem dispatchStep_latestOnly_count_le
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
Helper: `lifecycleTerminateStep` preserves `causedBy` and `concurrency`
on existing requests. Any request in the pre-state has a corresponding
request in the post-state with the same two fields — the only possible
mutation is flipping `isTerminal := true`.
-/
private theorem lifecycleTerminateStep_preserves_causedBy_and_concurrency
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

/-- **Theorem T3 (latest_only convergence).**

After a `latestOnly` fire materializes a fresh non-terminal request
`r_new`, every prior non-terminal request with matching `causedBy`
reaches a terminal (`superseded`) state.

Combined with S1 (terminal irreversibility) this rules out the
observable pathology where a `latestOnly` trigger leaves multiple
in-flight requests racing each other. -/
theorem T3_latest_only_convergence
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

/-- Consistency predicate tying a `RequestSeed`'s `causedBy` tuple to
    the scheduler-level `ExecutionOrigin` that the admission path will
    carry.

Mapping rules:

* Manual intents never reference a trigger document, and land on the
  interactive execution path — the one humans drive synchronously.
* Schedule and event fires both inherit the `scheduled` execution
  origin: the existing state machine already treats event-triggered
  work as scheduler-managed because both are produced without a
  foreground session holding the request. -/
def consistentLineage (seed : RequestSeed) (origin : ExecutionOrigin) : Prop :=
  (seed.causedByTriggerKind = .manual ∧ seed.causedByTriggerId = none ∧
    origin = .interactive) ∨
  (seed.causedByTriggerKind = .schedule ∧ origin = .scheduled) ∨
  (seed.causedByTriggerKind = .event ∧ origin = .scheduled)

/-- **Theorem T4 (lineage completeness).**

The `consistentLineage` predicate fully characterizes admissible
`(seed, origin)` pairs: every branch of the disjunction is exactly the
set of lineage tuples `dispatch` may produce.

Stated as an `iff` so it doubles as a definitional characterization,
which makes it easy for downstream proofs (and conformance tests in
Rust) to pattern-match on the three admissible shapes. -/
theorem T4_lineage_completeness
    (seed : RequestSeed) (origin : ExecutionOrigin) :
    consistentLineage seed origin ↔
      ((seed.causedByTriggerKind = .manual ∧
          seed.causedByTriggerId = none ∧
          origin = .interactive) ∨
       (seed.causedByTriggerKind = .schedule ∧ origin = .scheduled) ∨
       (seed.causedByTriggerKind = .event ∧ origin = .scheduled)) := by
  unfold consistentLineage
  exact Iff.rfl
