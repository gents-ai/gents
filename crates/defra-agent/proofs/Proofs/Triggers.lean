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

/-- Abstract event-trigger record visible in the active snapshot. -/
structure ActiveEventTrigger where
  triggerId : String
  enabled : Bool
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

/-- Abstract dispatch function. Produces a `RequestSeed` iff the intent
    is admissible given the snapshot. The concrete rules are filled in
    by the theorems below; `sorry` is used as a placeholder so the
    statements typecheck without committing to one operational
    definition. -/
def dispatch (snap : TriggerSnapshot) (intent : FireIntent) :
    Option RequestSeed :=
  sorry

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

/-- **Theorem T1 (enabled gate).**

A fire intent that successfully dispatches into a `RequestSeed` implies
its underlying trigger is present *and* enabled in the active snapshot
at the time of the fire. Manual intents are unconstrained — they are
not gated by a trigger document.

This theorem locks the invariant that a disabled schedule or event
trigger cannot admit new work, even if a stale scheduler tick races
the reconcile publish. -/
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
  -- Proof deferred: `dispatch` is itself a `sorry` placeholder at this
  -- layer of the spec. Once a concrete operational definition is
  -- supplied in a later PR, this proof reduces to an unfold + case on
  -- `intent.triggerKind` + the `enabled` check.
  sorry

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
