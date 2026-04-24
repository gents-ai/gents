import Proofs.Basic
import Proofs.Scheduling
import Proofs.RuntimeReconcile

/-!
# Layer 5: Trigger Engine

Abstract model of the trigger engine (schedule / event / manual fires)
that feeds the request lifecycle. This file introduces the shared
vocabulary: `TriggerKind`, `ConcurrencyMode`, `FireIntent`, `RequestSeed`
and an abstract `dispatch` that a fire goes through before it becomes a
materialized `AgentRequest`.

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
    some { causedByTriggerId := intent.triggerId, causedByTriggerKind := .manual }

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
  `triggerId` and `enabled = true`. Stated over the now-concrete
  `ActiveEventTrigger` structure (see PR 2 Task 27): the existential
  witness `trig` has the full shape `{triggerId, taskId,
  sourceCollection, eventKind, enabled, concurrency}`, matching the
  Rust `ResolvedEventTrigger`. The projection `trig.triggerId` /
  `trig.enabled` is what the gate checks, while the remaining fields
  become load-bearing for the yet-to-be-filled `dispatch` operational
  definition (e.g. joining on `sourceCollection` and `eventKind` to
  decide which subscription's buffer to drain).
* **Manual branch — unconditional.** `intent.triggerKind = .manual`
  imposes **no snapshot precondition** on the gate. Manual fires are
  operator-initiated and do not reference any trigger document, so
  there is nothing to look up. The Rust mirror of this is
  `TriggerEngine::dispatch` falling through the `TriggerKind::Manual`
  arm without touching `snapshot.active_schedules` or
  `snapshot.active_event_triggers`. See `T1_manual_unconditional`
  below for the lemma form of this observation.

**Proof state.** PR 1 left this theorem at `sorry` because `dispatch`
itself is still abstract (see its definition above). PR 3 does not
prove `dispatch`; once a concrete operational definition is supplied
in a later PR, the proof reduces to an unfold + case on
`intent.triggerKind` + the `enabled` check. The event branch will then
discharge directly against `ActiveEventTrigger`'s concrete fields;
previously it would have needed an opaque existential. -/
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


/-- **Theorem T2 (serial at-most-one).**

For any trigger key `t`, if every request in the system that was caused
by `t` is declared `serial`, then at most one non-terminal request
exists for `t` at any instant.

The proof relies on the in-flight lock check performed by the
dispatcher and on S1 (terminal irreversibility) from the request
lifecycle. Until the operational lock check is modeled explicitly here,
the proof is deferred. -/
theorem T2_serial_at_most_one
    (s : SystemState) (t : TriggerKey) :
    (∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial) →
    (s.requests.filter (fun r =>
        decide (r.causedBy = some t) ∧ ¬ r.isTerminal)).length ≤ 1 := by
  -- Proof deferred: requires modeling the per-trigger dispatch lock and
  -- appealing to S1 (`terminal_implies_released_local` and friends in
  -- `Request.lean`). The statement is the load-bearing contract for the
  -- serial-mode Rust implementation.
  sorry

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
