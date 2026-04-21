# Event-Driven Tasks Design

Status: draft — awaiting user review
Related issues: sourcenetwork/defra-agent#49 (Task/Schedule split), #9 (identity split — landed in schema, runtime pending)
Related specs: `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` (draft — not yet landed; this design integrates with the landed runtime half of that story and notes where it should extend the apply half when that lands)

## Summary

Replace today's `ScheduledTask` collection with three cooperating collections — `Task`, `Schedule`, and `EventTrigger` — plus a new runtime subsystem `TriggerEngine` that dispatches fires from any trigger source through a single materialization path. The engine consumes its trigger set from the existing `ActiveRuntimeSnapshot` generation-bump mechanism, so operator changes flow through the same reconcile loop as behaviors, backends, and other operator-controlled config. Adds gossip-driven event triggers as a first-class way for one agent's output to kick off another agent's work, while unifying cron schedules, event triggers, and manual runs behind one code path.

## Motivation

`ScheduledTask` bundles two concerns that should be separate: *what to run* (behavior, prompt, future task config) and *when to run it* (cron interval, next_run_at). Separating them unlocks:

- **Event-driven tasks.** The gossip-native workflow story: a trigger watches a document collection and runs a task when a matching document is created. This is how we get composable pipelines across nodes.
- **Reusable tasks.** One task, multiple schedules, or a mix of schedules plus event triggers.
- **First-class manual runs.** "Run this task now" becomes a unified path, not a special case.
- **One dispatch surface.** Concurrency modes, lineage fields, template rendering, failure bookkeeping live in one place instead of being duplicated across schedule and event code paths.
- **Reconcile consistency.** Operator edits to trigger config flow through the existing snapshot/generation mechanism just like every other config collection, instead of the runtime polling DB state directly for schedule fields.

## Architectural invariants (inputs to the design)

These shape the whole design. Any change here reshapes the rest.

1. **Deployment routing is 1:1 with `(agent_did, behavior_id)`.** Each defra-agent deployment has its own unique DID; a given `(did, behavior_id)` lives on exactly one deployment. Requests route deterministically to that one node.
   - Consequence: no cross-replica race to "own" a trigger fire. The deployment owning the referenced behavior is the sole evaluator.
   - Consequence: dedup keys on `AgentRequest` are for crash-recovery idempotency if ever needed, not for multi-node coordination.

2. **DefraDB documents are the control plane.** Configuration, triggers, requests, and responses all live as documents. The runtime reacts to documents; operators configure by writing documents; debugging is querying documents.

3. **Apply/runtime field ownership is per-field, not per-collection.** Each operator-controlled collection is ONE GraphQL type with apply-owned fields (written by the CLI's apply path) and runtime-owned fields (written by the runtime). The partition is informal today (distinguished by comment convention); a proposed spec aims to make it type-enforced. Non-interference is currently preserved by discipline.

4. **The reconcile loop is the canonical config-change path.** `control_watcher.rs` observes `EventName::Update` on operator-controlled collections, debounces, reloads `DocumentRuntimeView` from DB, resolves to `ResolvedRuntimeSnapshot`, publishes as `ActiveRuntimeSnapshot` with a bumped generation counter. Downstream consumers read from the snapshot, not directly from the DB. This design plugs into that existing flow; it does not introduce a parallel config-watch path.

## Apply / reconcile integration

Task, Schedule, and EventTrigger join the set of operator-controlled collections (bringing the total from seven to ten). They follow the existing pattern end to end.

**CLI-side (manifest parsing):** three new Rust structs in `crates/defra-agent-cli/src/desired_state/mod.rs` — `DesiredTask`, `DesiredSchedule`, `DesiredEventTrigger` — following the shape of the existing seven (`DesiredAgentBehavior`, `DesiredInferenceBackend`, etc.). The CLI's diff-manifest-against-live-state pipeline gains three branches for these collections. Apply writes only apply-owned fields; runtime-owned fields are never touched by apply.

**Control watcher:** the watched collection set gains `Task`, `Schedule`, `EventTrigger`. Updates to any of them fire the existing 5s debounce and trigger a snapshot reload.

**DocumentRuntimeView:** extended with three fields — `tasks: Map<TaskId, Task>`, `schedules: Map<ScheduleId, Schedule>`, `event_triggers: Map<TriggerId, EventTrigger>` — populated from `load_document_runtime_view()`.

**ResolvedRuntimeSnapshot:** gains the three corresponding maps, but with the Task joined in so the engine can fire without re-joining at fire time:

```rust
struct ResolvedSchedule { schedule_id, task_id, task: ResolvedTask,
                          interval_secs, enabled, concurrency, /* runtime fields */ }
struct ResolvedEventTrigger { trigger_id, task_id, task: ResolvedTask,
                              source_collection, event_kind, filter, enabled, concurrency,
                              /* runtime fields */ }
struct ResolvedTask { task_id, behavior_id, prompt_template, output_schema_ref }
```

A snapshot whose referenced task doesn't exist or is disabled puts the schedule/trigger into the `unavailable` set, matching existing behavior for a schedule that points at an unresolvable behavior.

**ActiveRuntimeSnapshot:** the published form exposes `active_schedules()` and `active_event_triggers()` that the `TriggerEngine` sources consume. Generation semantics extend naturally: a fire materialized against generation N records the snapshot it saw; a new generation N+1 publish replaces the engine's view for future fires without disturbing in-flight ones.

**ApplyReconcile.lean dependency:** the draft spec at `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` proposes a `Collection` enum (Lean inductive + Rust counterpart) and a typed `DesiredFields`/`LiveFields` partition. When that work lands, Task/Schedule/EventTrigger should be added as three new `Collection` variants, and the T-Conv convergence theorem extends to them without new proof work. This design does NOT block on that spec landing — the runtime integration uses the already-landed `RuntimeReconcile.lean` + control-watcher machinery directly.

## The three collections

### `Task`

Reusable unit of work. No opinion about when or why it runs. Entirely apply-owned (no runtime-owned fields).

```graphql
type Task @branchable {
    # apply-owned (all):
    task_id: String @index(unique: true)
    name: String @index
    description: String
    behavior_id: String @index            # resolves principal via AgentBehavior
    prompt_template: String               # template string, rendered at fire time
    enabled: Boolean @index               # operator-level kill switch
    output_schema_ref: String             # reserved; null in v1 (future: task declares output type)
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

No fire bookkeeping on `Task`. Aggregate run history is a query across `AgentRequest` filtered by the `(caused_by_trigger_id, caused_by_trigger_kind)` tuple for the triggers pointing at this task.

`Task` references only `behavior_id`. `AgentBehavior` already carries `agent_did`, so principal/deployment resolve via a single lookup. No `agent_did` denormalization on `Task` — single source of truth is `AgentBehavior`.

### `Schedule`

Cron-style trigger.

```graphql
type Schedule @branchable {
    # apply-owned:
    schedule_id: String @index(unique: true)
    task_id: String @index
    interval_secs: Int
    enabled: Boolean @index
    concurrency: String @index            # parallel | serial | latest_only  (default: serial)
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
    # runtime-owned (written by TriggerEngine callbacks):
    next_run_at: DateTime @index(direction: ASC)
    last_attempt_at: DateTime @index(direction: DESC)
    last_status: String @index            # see FireAttemptStatus enum below
    last_error: String
    fire_count: Int
}
```

### `EventTrigger`

Document-event trigger. Fires when a matching document appears in the watched collection.

```graphql
type EventTrigger @branchable {
    # apply-owned:
    trigger_id: String @index(unique: true)
    task_id: String @index
    source_collection: String @index      # e.g. "CustomerSignup"
    event_kind: String @index             # "created" in v1; extensible (update/delete future)
    filter: String                        # optional GraphQL filter; empty = match-all
    enabled: Boolean @index
    concurrency: String @index            # same enum as Schedule
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
    # runtime-owned:
    last_attempt_at: DateTime @index(direction: DESC)
    last_fired_source_doc_id: String
    last_status: String @index            # see FireAttemptStatus enum below
    last_error: String
    fire_count: Int
}
```

v1 ships `event_kind = "created"` only. The field is present now so update/delete can be added later without a schema break.

### `FireAttemptStatus` enum (string-valued)

Documented once; used by both `Schedule.last_status` and `EventTrigger.last_status`.

- `"fired"` — the fire attempt successfully materialized an `AgentRequest`. Does NOT imply the run completed. The authoritative run outcome lives on `AgentRequest.lifecycle_state`.
- `"skipped"` — the fire attempt was intentionally dropped (typically by `serial` concurrency mode when a prior fire is still in-flight). No `AgentRequest` was created.
- `"error"` — the fire attempt failed before materialization (template render failure, `behavior_id` resolution failure, etc.). No `AgentRequest` was created. `last_error` carries the message.

`fire_count` increments only on `"fired"`. `last_attempt_at` updates on every attempt regardless of status.

### `AgentRequest` additions

Two new fields, populated at materialization time by the engine:

```graphql
caused_by_trigger_id: String @index       # null for manual; schedule_id or trigger_id otherwise
caused_by_trigger_kind: String @index     # "schedule" | "event" | "manual"
```

**Namespace rule.** `schedule_id` and `trigger_id` live in separate collections with independent unique-ID namespaces — nothing prevents a collision. All lineage lookups and concurrency-mode in-flight queries match on the tuple `(caused_by_trigger_id, caused_by_trigger_kind)`, not on `caused_by_trigger_id` alone.

These fields are for lineage and observability — "why did this request run?" — and power the engine's own concurrency-mode in-flight queries. Deliberately *not* a hop counter (see Loop Prevention below).

## Runtime: `TriggerEngine` + `TriggerSource` trait

One subsystem dispatches fires from all sources through a single materialization path. Sources consume from the published `ActiveRuntimeSnapshot`, not from DB polls.

```rust
trait TriggerSource: Send + Sync {
    fn next_fire(&mut self) -> impl Future<Output = Option<FireIntent>> + Send;
}

struct FireIntent {
    trigger_id: Option<String>,           // None for manual
    trigger_kind: TriggerKind,            // Schedule | Event | Manual
    task: ResolvedTask,                   // already-joined task fields from the snapshot
    concurrency: ConcurrencyMode,
    event_vars: serde_json::Value,        // {{event.*}} scope
    doc_vars: Option<serde_json::Value>,  // {{doc.*}} scope — None for schedule/manual
    args_vars: Option<serde_json::Value>, // {{args.*}} scope — Some for manual, None otherwise
    on_result: Box<dyn FnOnce(FireResult) + Send>,  // source-specific bookkeeping callback
}

struct TriggerEngine {
    snapshot: Arc<RwLock<ActiveRuntimeSnapshot>>,
    materializer: MaterializerHandle,
}

impl TriggerEngine {
    async fn run(self, sources: Vec<Box<dyn TriggerSource>>) {
        // Drive all sources concurrently (FuturesUnordered), funneling into one dispatch path.
        // For each FireIntent:
        //   1. Enabled gate: re-check against current snapshot (trigger & task still enabled).
        //   2. Render task.prompt_template with (event_vars, doc_vars, args_vars).
        //   3. Concurrency-mode decision vs in-flight requests, matching on
        //      (caused_by_trigger_id, caused_by_trigger_kind). For latest_only, hold a
        //      per-trigger async lock around supersede + materialize so two fires cannot
        //      interleave.
        //   4. Materialize AgentRequest (or skip / supersede).
        //   5. Call on_result for source-specific trigger-doc updates (runtime-owned fields only).
    }
}
```

**Three concrete sources:**

- **`ScheduleSource`** — holds a reference to the snapshot. On each tick (1s granularity), reads `snapshot.active_schedules()`, finds schedules with `next_run_at <= now` (reading the runtime-owned `next_run_at` from DB since the snapshot's copy is generation-N-stale relative to recent runtime writes — see "Runtime-owned field reads" below), and yields `FireIntent`. Callback advances `next_run_at += interval_secs`, updates `last_attempt_at`, `last_status`, `fire_count` on the `Schedule` doc. Produces `FireIntent` with `event_vars` only (`doc_vars = None`, `args_vars = None`).

- **`EventSource`** — holds a reference to the snapshot. On snapshot publish, reconciles its underlying `defra-node::events::subscribe()` subscriptions to match the set of `source_collection` values across `snapshot.active_event_triggers()`. One subscription per unique collection; many triggers may share it. On each `EventName::Update` for `(collection C, docID D)`, consults the current snapshot to find triggers with `source_collection = C`, evaluates each trigger's filter via `Collection(filter: { _docID: { _eq: D }, AND: trigger.filter }, limit: 1)`, fetches the doc for `doc_vars`, and yields `FireIntent`. Callback updates `last_attempt_at`, `last_fired_source_doc_id`, `last_status`, `fire_count` on the `EventTrigger` doc.

- **`ManualSource`** — mpsc channel that the CLI/desktop push into. `FireIntent` carries caller-supplied `args_vars`. No trigger document; callback is a no-op.

All three paths end at the same `materialize_claimed_with_execution_binding()` call that exists today in `lifecycle/materialize.rs`.

### Generation semantics

- A fire that begins under generation N completes against the `ResolvedTask` it captured at fire time. In-flight requests are not retroactively rebound when a new snapshot publishes.
- On generation N+1 publish:
  - `ScheduleSource` switches to the new `active_schedules()` on its next tick. Schedules removed in N+1 stop firing; schedules added in N+1 begin firing on their own `next_run_at`.
  - `EventSource` reconciles its subscription set to the new `source_collection`s. Newly-added triggers immediately match fresh events. Removed triggers stop matching.
- In-flight request completion paths are generation-agnostic — they use the request's own bound state.

### Runtime-owned field reads

The snapshot is the canonical source for apply-owned fields (`enabled`, `interval_secs`, `filter`, `concurrency`, the task config). Runtime-owned fields (`next_run_at`, `last_attempt_at`, `last_status`, `fire_count`, `last_error`, `last_fired_source_doc_id`) are written by the engine directly to the DB and read from the DB on a cadence — the snapshot's copy of these is not load-bearing. This matches existing runtime-writes-live-state-directly behavior for e.g. `InferenceBackend.probe_status`.

Concretely: `ScheduleSource` reads `next_run_at` from DB per tick (indexed query), not from the snapshot's cached copy. Rationale: next_run_at changes rapidly under runtime pressure, debouncing a snapshot republish for every runtime write would churn the generation counter.

## Template engine

**Choice:** MiniJinja. Pure-Rust Jinja2, actively maintained, sandboxed by default (no filesystem/network/env access), small footprint. Tera is an acceptable fallback; Handlebars is too logic-less; Askama is compile-time.

**Variable scopes:**

| Variable | Schedule | Event | Manual |
|---|:---:|:---:|:---:|
| `event.fired_at` | ✓ | ✓ | ✓ |
| `event.trigger_id` | ✓ | ✓ | null |
| `event.trigger_kind` | `"schedule"` | `"event"` | `"manual"` |
| `event.source_collection` | — | ✓ | — |
| `event.source_doc_id` | — | ✓ | — |
| `doc.<field>` | — | ✓ | — |
| `args.<key>` | — | — | ✓ |

### Pre-validation — trigger-apply time, not task-apply time

Validation ownership: each trigger owns its own scope contract and checks the referenced task's template against that scope at trigger apply time. This lets Task be applied before any trigger references it (common apply order) without failing on `{{doc.*}}` references intended for a future event trigger.

**At `Task` apply:** parse the `prompt_template` with MiniJinja's parser. Check syntactic validity. Check variable roots are in the allowed set (`event`, `doc`, `args`). No semantic field resolution. Reject only on syntax error or unknown variable root.

**At `Schedule` apply:** load the referenced `Task`. Render the template against the schedule scope (`event.*` only). Reject apply if the template references `doc.*` or `args.*`.

**At `EventTrigger` apply:** load the referenced `Task`. Resolve the `source_collection`'s GraphQL schema. Render the template against the event scope (`event.*` + `doc.*`). For each `doc.<path>` reference, check the field path exists in the source collection's type. Reject apply if anything fails. Also issue a probe query against `source_collection` with the filter and `limit: 0` — DefraDB returns a syntax error for malformed filters; reject apply on syntax error with the DefraDB message verbatim.

**Manual runs:** no apply-time validation. The caller supplies `args` at runtime; mismatches produce a runtime render failure (see below).

### Runtime behavior

- MiniJinja configured with `undefined_behavior = Strict` and `auto_escape = None` (output is an LLM prompt, not HTML).
- On render failure, the fire aborts before materialization. The trigger document records `last_status = "error"`, `last_error = <engine message>`, `last_attempt_at = now`. `fire_count` is not incremented. `Schedule.next_run_at` advances normally so we don't loop on the same broken fire; `EventTrigger` does not retry the failed doc — the next matching doc gets a fresh attempt.
- Size caps: 64 KB per `prompt_template` (apply time), 1 MB per rendered `AgentRequest.content` (runtime).

## Concurrency modes

Three modes per trigger. Enforced by the engine immediately before materialization, using `(caused_by_trigger_id, caused_by_trigger_kind)` to find in-flight requests.

**`parallel`.** No coordination. Every matching event materializes a new request. Subject only to the existing inference-admission capacity limiter.

**`serial`** *(default)*. Query `AgentRequest` for any non-terminal row with `(caused_by_trigger_id = T, caused_by_trigger_kind = K)`. If one exists, skip this fire: `last_status = "skipped"`, `last_error = "serial: prior fire still in-flight"`, `last_attempt_at = now`, `fire_count` unchanged. For `Schedule`, `next_run_at` advances normally on skip (we do not hold the clock waiting for the in-flight run; the next tick tries again). For `EventTrigger`, the skipped doc is not re-evaluated. The event is dropped, not queued. Explicit v1 semantics: "best-effort in-order, at most one fire in flight at a time." Strict queuing would need a new lifecycle state; deferred until real usage demands it.

**`latest_only`.** Holds a per-trigger async lock for the duration of the supersede-then-materialize pair so two fires for the same trigger cannot interleave. Inside the lock: find non-terminal requests with matching `(caused_by_trigger_id, caused_by_trigger_kind)` and transition each to `Superseded` (existing terminal state in the request lifecycle — no state machine change needed); then materialize the new fire. Uses S1 (terminal irreversibility) cleanly. The lock is in-memory; per architectural invariant 1, only one deployment evaluates a given trigger, so no distributed lock is needed.

## Backfill and ordering

**Forward-only.** When an operator creates or enables a trigger, it fires only for docs that appear after the trigger is active. Historical state is not replayed. This avoids the operational footgun of "enabling a trigger kicks off 10,000 LLM runs against a seeded collection." Operators who need one-off runs against historical docs use the manual path.

**Arrival order at the local node.** Docs fire in the order the local gossip subscription receives them. No attempt at global ordering — DefraDB's gossip doesn't provide it, and each deployment processes what it sees locally.

**Deployment startup / restart.** On startup, `EventSource` subscribes at the current moment (not from a persisted cursor). Events that arrived during downtime are not replayed. This extends the "forward-only" semantic to restarts. The bounded-poll fallback (referenced in the testing strategy) compensates only for *live* subscription drops detected via `check_and_reset_dropped()` — it does not compensate for restart gaps.

## Loop prevention

Not enforced at the system level beyond locality and existing safety nets.

- **Locality:** an event trigger fires only when the deployment has the source doc *and* owns the referenced behavior. Each link in a potential cycle is scoped to one deployment's routing.
- **Existing safety nets:** `AgentRequest.max_retries`, `AgentRequest.deadline`, per-backend inference-admission caps, operator kill switch (`trigger.enabled = false`).
- **Observability:** `fire_count` and `last_attempt_at` on the trigger, plus `(caused_by_trigger_id, caused_by_trigger_kind)` lineage on every request, make runaway behavior visible.

Cycles are possible (operator mistake) but bounded per unit time by admission and recoverable by disabling the trigger. We document this explicitly rather than building hop-counter machinery that would require stamping request IDs on every tool write.

## Lean proof extensions

Existing proofs (request lifecycle S1–S6, liveness L1/L3, scheduler capacity invariant, `RuntimeReconcile.lean` coherence) stay intact — nothing in this design reshapes those state machines.

**New file: `crates/defra-agent/proofs/Proofs/Triggers.lean`.** Defines `TriggerKind`, `ConcurrencyMode`, `FireIntent`, and a `dispatch : ActiveRuntimeSnapshot → FireIntent → Option RequestSeed` function modeling the engine's fire path. The snapshot parameter ties the proof to `RuntimeReconcile.lean`'s coherence invariants — properties are stated against the published snapshot, not against raw DB state.

Template rendering is modeled as a pure function `render : Template → Scope → Either Error String`; it slots into the proof as a deterministic oracle.

**Properties to prove:**

- **T1 (enabled gate via snapshot).** `dispatch` returns `Some` only when both the trigger and its referenced task are in the `enabled` set of the supplied snapshot. Follows from the fire path's precondition check against `snapshot.active_schedules()` / `active_event_triggers()`, composed with `RuntimeReconcile`'s guarantee that the snapshot is a coherent projection of the current manifest.
- **T2 (serial at-most-one).** Under `Serial`, for any trigger `(id, kind)`, `|{ r : AgentRequest | r.caused_by = (id, kind) ∧ ¬r.isTerminal }| ≤ 1` at all times. Stated as a pair match, consistent with the namespace rule. Follows from the in-flight check + S1 (terminal irreversibility).
- **T3 (latest_only convergence).** Under `LatestOnly`, after a fire materializes `r_new`, all prior `r_prior` with matching `caused_by` reach `Superseded` (terminal). Relies on the per-trigger async lock serializing supersede-then-materialize pairs + S1.
- **T4 (lineage completeness).** Every `AgentRequest` has `(caused_by_trigger_id, caused_by_trigger_kind)` consistent with its `execution_origin`: `manual ↔ (null, "manual")`, `scheduled ↔ (schedule_id, "schedule")`, `event ↔ (trigger_id, "event")`.

**Explicit non-properties:**

- **No hop-bound proof.** Cycles are operator-observable and bounded by existing L1/admission proofs only. Documented in the Lean spec as an intentional omission.
- **No strict-serial queue proof.** `Serial = skip` is v1 semantics; we don't claim "every matching doc fires exactly once."

**Future interaction with `ApplyReconcile.lean`.** When the draft apply-reconcile spec lands (adding `Collection` enum, typed `DesiredFields`/`LiveFields`, T-Conv theorem), Task/Schedule/EventTrigger become three new `Collection` variants. T-Conv — "apply a manifest, reconcile runs to quiescence, the snapshot's runnable set equals the manifest's" — extends automatically to cover the trigger collections; `Triggers.lean`'s properties, phrased against the snapshot, compose cleanly with it. No retroactive proof rewrite is needed.

**Conformance tests.** `tests/state_machine_conformance.rs` gains serial-skip, latest_only-supersede, and template-render-failure cases (all three paths reachable via `Schedule` alone in PR 1). New files `tests/trigger_conformance.rs` (PR 2) and `tests/schedule_conformance.rs` (PR 1) cover end-to-end fire behavior against a freshly-published snapshot. The existing `lifecycle_regression.rs` picks up new `Superseded` sources from `latest_only`.

## Rollout

Three PRs landing in order. Each is independently shippable and reviewable.

### PR 1 — Task/Schedule split + TriggerEngine scaffold + reconcile integration

- New schemas: `Task`, `Schedule`. Remove `ScheduledTask` schema outright.
- New CLI Rust structs: `DesiredTask`, `DesiredSchedule` in `desired_state/mod.rs`. Diff/apply branches extended.
- New fields on `AgentRequest`: `caused_by_trigger_id`, `caused_by_trigger_kind` (indexed on both; queries always match on the tuple).
- Extend `DocumentRuntimeView`, `ResolvedRuntimeSnapshot`, `ActiveRuntimeSnapshot` with `tasks` / `schedules` (+ `event_triggers` as an empty map for now, so PR 2 only adds population logic).
- Extend `control_watcher.rs` to subscribe to `Task` and `Schedule` collection updates.
- Build `TriggerEngine` + `TriggerSource` trait + `ScheduleSource`.
- Retarget today's `Scheduler::run()` call site to `TriggerEngine::run(vec![ScheduleSource::new(snapshot.clone())])`. Cron behavior is unchanged from the outside.
- MiniJinja dependency + template module (used by `ScheduleSource` from day one — today's raw-string `ScheduledTask.prompt` becomes a template that still renders as raw text when no variables are referenced).
- Pre-validation at `Task`, `Schedule` apply time (template checks for schedule scope).
- Lean: extend `Scheduling.lean` as needed; new `Triggers.lean`; prove T1, T2, T3, T4 for the schedule path.
- Conformance: new `tests/schedule_conformance.rs`; extend `tests/state_machine_conformance.rs` with serial-skip, latest_only-supersede, template-render-failure cases.
- Desktop UI retargeted to show `Task` + `Schedule` instead of `ScheduledTask`.

No migration. Operators re-create their schedules through the apply flow (clean break; existing `ScheduledTask` apply manifests are rewritten to `Task` + `Schedule` pairs by hand — mechanical mapping). An optional `defra-agent-cli wipe scheduled-tasks` helper may ship alongside PR 1 to clear orphaned rows; otherwise operators leave them as dead data until the collection is removed from the DB.

### PR 2 — EventTrigger + EventSource

- New schema: `EventTrigger`. New CLI Rust struct: `DesiredEventTrigger`.
- Populate `snapshot.event_triggers` from the extended `DocumentRuntimeView`; extend `control_watcher.rs` to watch `EventTrigger`.
- Add `EventSource` implementing `TriggerSource`; engine registers it alongside `ScheduleSource`.
- Pre-validation at `EventTrigger` apply: template's `doc.*` references resolve against `source_collection` schema; filter syntax probed via `limit: 0` query.
- Lean: extend `Triggers.lean`; prove T1–T4 for the event path.
- Conformance: `tests/trigger_conformance.rs`.

### PR 3 — Manual runs + operator ergonomics

- `ManualSource` + `run_task_now(task_id, args)` helper wired to CLI and desktop.
- Desktop observability polish: `last_attempt_at`, `fire_count`, `last_error` surfaced on Task detail views; lineage badges on requests showing their trigger origin.
- CLAUDE.md updates covering the trigger model; apply-manifest doc updates.

## Testing strategy

**Unit tests (colocated `<module>/tests.rs`):**

- Template rendering: scope isolation (schedule templates can't reference `doc.*`), strict-undefined behavior, size caps.
- Filter pre-validation via `limit: 0` probe against `source_collection`.
- Concurrency-mode dispatch decisions with mocked in-flight queries (tuple-matched lookups).
- Lineage field population in the materializer (all three origins).
- `ResolvedSchedule` / `ResolvedEventTrigger` build logic: unresolvable task → `unavailable` set.

**Conformance tests (`crates/defra-agent/tests/`):**

- `schedule_conformance.rs` (PR 1): fire at `next_run_at`, enabled gate, template render failure records `last_error`, serial skip under in-flight, `latest_only` supersedes, generation-N+1 reconfigures the source on next tick.
- `trigger_conformance.rs` (PR 2): filter match/miss, enabled gate, template failure, backfill is forward-only (seed docs before trigger exists, enable trigger, write new doc, verify only the new doc fires), subscription reconciliation on snapshot publish.

**Property tests:** the `apply_property.rs` tests proposed in the ApplyReconcile draft aren't blocked on that spec landing — PR 1 may optionally add property-test coverage for the new Task/Schedule diff+apply paths if the user wants.

**Soak test (extend existing infrastructure):** K schedules + M event triggers pointing at L tasks; generate load by writing matching docs at rate R; run for N minutes; assert `fire_count` monotone, no stuck non-terminal requests, subscription drops compensated by the bounded-poll fallback, snapshot-generation bumps handled cleanly when triggers are edited under load.

## Risk register

| Risk | Mitigation |
|---|---|
| MiniJinja binary-size or footprint too high | Confirm during PR 1; fall back to Tera or a minimal substituter if unacceptable |
| Subscription refresh storms when many triggers land at once | Debounce refresh in `EventSource` on snapshot publish (coalesce within ~250ms) |
| Operator forgets to rewrite apply manifests from `ScheduledTask` | Detect during apply; emit a specific error pointing at the field-mapping guide |
| Template validation against large collection schemas is slow | Cache parsed schemas in the engine; invalidate on schema reload |
| Runaway cycle consumes inference budget | Existing admission caps bound spend-per-unit-time; operator disables the trigger |
| Runtime-owned field writes conflict with apply writes | Covered by non-interference discipline today; will be type-enforced when `ApplyReconcile.lean`'s `DesiredFields`/`LiveFields` split lands |

## Out of scope (explicit)

- **Update / delete event kinds.** `event_kind` field reserved; v1 ships `"created"` only.
- **Hop counter / request-write provenance.** Deferred indefinitely pending concrete operational need.
- **Strict-serial queuing.** `Serial = skip` is v1 semantics.
- **Task-specific output schemas.** `output_schema_ref` field reserved; the typed-pipeline story lands separately.
- **`ScheduledTask` migration tooling.** Clean break; manifests are rewritten by hand (optional `wipe` helper noted in PR 1).
- **Full `AgentPrincipal`/`AgentBehavior` runtime split (issue #9).** `Task.behavior_id` is the only identity reference; the rest of the runtime's identity refactor is orthogonal to this spec.
- **`ApplyReconcile.lean` / `Collection` enum / typed `DesiredFields`-`LiveFields` split.** These are a separate pending workstream (see its design doc). When they land, the three new collections join the enum and T-Conv automatically covers them — but this spec's implementation does not block on that work.
