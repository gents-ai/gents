# Event-Driven Tasks Design

Status: draft — awaiting user review
Related issues: sourcenetwork/defra-agent#49 (Task/Schedule split), #9 (identity split — landed in schema, runtime pending)

## Summary

Replace today's `ScheduledTask` collection with three cooperating collections — `Task`, `Schedule`, and `EventTrigger` — plus a new runtime abstraction `TriggerEngine` that dispatches fires from any trigger source through a single materialization path. Adds gossip-driven event triggers as a first-class way for one agent's output to kick off another agent's work, while unifying cron schedules, event triggers, and manual runs behind one code path.

## Motivation

`ScheduledTask` bundles two concerns that should be separate: *what to run* (behavior, prompt, future task config) and *when to run it* (cron interval, next_run_at). Separating them unlocks:

- **Event-driven tasks.** The gossip-native workflow story: a trigger watches a document collection and runs a task when a matching document is created. This is how we get composable pipelines across nodes.
- **Reusable tasks.** One task, multiple schedules, or a mix of schedules plus event triggers.
- **First-class manual runs.** "Run this task now" becomes a unified path, not a special case.
- **One dispatch surface.** Concurrency modes, lineage fields, template rendering, failure bookkeeping live in one place instead of being duplicated across schedule and event code paths.

## Architectural invariants (inputs to the design)

These shape the whole design. Any change here reshapes the rest.

1. **Deployment routing is 1:1 with `(agent_did, behavior_id)`.** Each defra-agent deployment has its own unique DID; a given `(did, behavior_id)` lives on exactly one deployment. Requests route deterministically to that one node. This is not a coordination mechanism we build — it's how the system already works.
   - Consequence: there is no cross-replica race to "own" a trigger fire. The deployment that owns the referenced behavior is the sole evaluator.
   - Consequence: "dedup keys" on `AgentRequest` are for crash-recovery idempotency if ever needed, not for multi-node coordination.

2. **DefraDB documents are the control plane.** Configuration, triggers, requests, and responses all live as documents. The runtime reacts to documents; operators configure by writing documents; debugging is querying documents.

3. **Field ownership separates apply-path (desired state) from runtime (live state).** Neither clobbers the other.

## The three collections

### `Task`

Reusable unit of work. No opinion about when or why it runs.

```graphql
type Task @branchable {
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

No fire bookkeeping on `Task`. Aggregate run history is a query across `AgentRequest` filtered by `caused_by_trigger_id` for the triggers pointing at this task.

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
    # runtime-owned:
    next_run_at: DateTime @index(direction: ASC)
    last_fired_at: DateTime @index(direction: DESC)
    last_status: String @index            # fired | skipped | error
    last_error: String
    fire_count: Int
    # meta:
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
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
    # runtime-owned:
    last_fired_at: DateTime @index(direction: DESC)
    last_fired_source_doc_id: String
    last_status: String @index
    last_error: String
    fire_count: Int
    # meta:
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

v1 ships `event_kind = "created"` only. The field is present now so update/delete can be added later without a schema break.

### `AgentRequest` additions

Two new fields, populated at materialization time by the engine:

```graphql
caused_by_trigger_id: String @index       # schedule_id | trigger_id | null (manual)
caused_by_trigger_kind: String @index     # "schedule" | "event" | "manual"
```

These are for lineage and observability — "why did this request run?" — and power the engine's own concurrency-mode in-flight queries. Deliberately *not* a hop counter (see Loop Prevention below).

## Runtime: `TriggerEngine` + `TriggerSource` trait

One subsystem dispatches fires from all sources through a single materialization path.

```rust
trait TriggerSource: Send + Sync {
    fn next_fire(&mut self) -> impl Future<Output = Option<FireIntent>> + Send;
}

struct FireIntent {
    trigger_id: Option<String>,           // None for manual
    trigger_kind: TriggerKind,            // Schedule | Event | Manual
    task_id: String,
    concurrency: ConcurrencyMode,
    event_vars: serde_json::Value,        // {{event.*}} scope
    doc_vars: Option<serde_json::Value>,  // {{doc.*}} scope — None for schedule/manual
    on_result: Box<dyn FnOnce(FireResult) + Send>,  // source-specific bookkeeping callback
}

struct TriggerEngine { /* holds sources + materializer handle */ }

impl TriggerEngine {
    async fn run(self, sources: Vec<Box<dyn TriggerSource>>) {
        // select! over sources; for each FireIntent:
        //   1. Load Task (template, behavior_id, enabled gate).
        //   2. Render prompt_template with (event_vars, doc_vars).
        //   3. Concurrency-mode decision vs existing in-flight requests.
        //   4. Materialize AgentRequest (or skip / supersede).
        //   5. Call on_result for source-specific trigger-doc updates.
    }
}
```

Three concrete sources:

- **`ScheduleSource`** — 60s poll loop over enabled `Schedule` rows where `next_run_at <= now`. Produces `FireIntent` with `event_vars` only. Callback advances `next_run_at`, updates `last_fired_at`, `last_status`, `fire_count` on the `Schedule` doc.
- **`EventSource`** — DefraDB gossip subscription. One underlying `EventName::Update` subscription per unique `source_collection` with ≥1 enabled trigger; a control-plane watch on `EventTrigger` refreshes the subscription set when triggers are added/removed. On update for `(collection, docID)`, finds matching enabled triggers, evaluates filter via `Collection(filter: { _docID: { _eq: D }, AND: trigger.filter }, limit: 1)`, fetches doc fields for `{{doc.*}}` scope. Callback updates `last_fired_at`, `last_fired_source_doc_id`, etc.
- **`ManualSource`** — mpsc channel that the CLI/desktop push into. `FireIntent` carries caller-supplied `args` (exposed in templates as `{{args.*}}`). No trigger document; callback is a no-op.

All three paths end at the same `materialize_claimed_with_execution_binding()` call that exists today in `lifecycle/materialize.rs`.

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

**Pre-validation (apply time):**

When a `Task` is applied, parse the `prompt_template` with MiniJinja's parser, collect variable references, and validate against every scope the task could be fired in — i.e., against each trigger pointing at the task. If any schedule points at the task, the template must render with `event.*` alone. If any event trigger points at the task, the template must also render with `doc.*` fields from that trigger's `source_collection` schema. References to `args.*` are allowed unconditionally (manual-run contract is caller-defined). Unresolvable references fail the apply with a specific error (variable name + which scope check failed).

Validation is at the `Task` level, not the trigger level. A task must render for every trigger that points at it. Operators who want trigger-specific prompts write separate tasks.

**Filter pre-validation (EventTrigger, apply time):**

When an `EventTrigger` with a non-empty `filter` is applied, issue a probe query against the `source_collection` using the filter with `limit: 0`. DefraDB returns a syntax error if the filter is malformed; a successful probe (empty or non-empty result set either way) confirms the filter parses and references valid fields. Reject the apply on syntax error with the DefraDB error verbatim.

**Runtime behavior:**

- MiniJinja configured with `undefined_behavior = Strict` and `auto_escape = None` (output is an LLM prompt, not HTML).
- On render failure, the fire aborts before materialization. The trigger document records `last_status = "error"`, `last_error = <engine message>`, `last_fired_at = now`. `fire_count` is not incremented. `Schedule.next_run_at` advances normally so we don't loop on the same broken fire; `EventTrigger` does not retry the failed doc — the next matching doc gets a fresh attempt.
- Size caps: 64 KB per `prompt_template` (apply time), 1 MB per rendered `AgentRequest.content` (runtime).

## Concurrency modes

Three modes per trigger. Enforced by the engine immediately before materialization, using the lineage fields to find in-flight requests.

**`parallel`.** No coordination. Every matching event materializes a new request. Subject only to the existing inference-admission capacity limiter.

**`serial`** *(default)*. Query `AgentRequest` for any non-terminal row with `caused_by_trigger_id = <this trigger>`. If one exists, skip this fire: `last_status = "skipped"`, `last_error = "serial: prior fire still in-flight"`, `last_fired_at = attempt time`, `fire_count` unchanged. For `Schedule`, `next_run_at` advances normally on skip (we do not hold the clock waiting for the in-flight run; the next tick tries again). For `EventTrigger`, the skipped doc is not re-evaluated. The event is dropped, not queued. This is explicit v1 semantics — "best-effort in-order, at most one fire in flight at a time." Strict queuing would need a new lifecycle state; deferred until real usage demands it.

**`latest_only`.** Find non-terminal requests with the same `caused_by_trigger_id` and transition each to `Superseded` (existing terminal state in the request lifecycle — no state machine change needed). Then materialize the new fire. Uses S1 (terminal irreversibility) cleanly.

## Backfill and ordering

**Forward-only.** When an operator creates or enables a trigger, it fires only for docs that appear after the trigger is active. Historical state is not replayed. This avoids the operational footgun of "enabling a trigger kicks off 10,000 LLM runs against a seeded collection." Operators who need one-off runs against historical docs use the manual path.

**Arrival order at the local node.** For serial mode and for general ordering, docs fire in the order the local gossip subscription receives them. No attempt at global ordering — DefraDB's gossip doesn't provide it, and each deployment processes what it sees locally.

## Loop prevention

Not enforced at the system level beyond locality and existing safety nets.

- **Locality:** an event trigger fires only when the deployment has the source doc *and* owns the referenced behavior. Each link in a potential cycle is scoped to one deployment's routing.
- **Existing safety nets:** `AgentRequest.max_retries`, `AgentRequest.deadline`, per-backend inference-admission caps, operator kill switch (`trigger.enabled = false`).
- **Observability:** `fire_count` and `last_fired_at` on the trigger, plus `caused_by_trigger_id` lineage on every request, make runaway behavior visible.

Cycles are possible (operator mistake) but bounded per unit time by admission and recoverable by disabling the trigger. We document this explicitly rather than building hop-counter machinery that would require stamping request IDs on every tool write.

## Lean proof extensions

Existing proofs (request lifecycle S1–S6, liveness L1/L3, scheduler capacity invariant) stay intact — nothing in this design reshapes the request state machine.

**New file: `crates/defra-agent/proofs/Proofs/Triggers.lean`.** Defines `TriggerKind`, `ConcurrencyMode`, `FireIntent`, and a `dispatch : FireIntent → RequestSeed` function modeling the engine's fire path. Template rendering is a pure function (`render : Template → Scope → Either Error String`), so it slots into the proof as a deterministic oracle.

**Properties to prove:**

- **T1 (enabled gate).** An `AgentRequest` with `caused_by_trigger_id = t` is materialized only when `t.enabled = true` at fire time.
- **T2 (serial at-most-one).** Under `Serial`, `|{ r : AgentRequest | r.caused_by_trigger_id = t ∧ ¬r.isTerminal }| ≤ 1` at all times. Follows from the in-flight check + S1.
- **T3 (latest_only convergence).** Under `LatestOnly`, after a fire materializes `r_new`, all prior `r_prior` with the same `caused_by_trigger_id` reach `Superseded` (terminal). Uses S1 + atomic supersede-then-materialize.
- **T4 (lineage completeness).** Every `AgentRequest` has `(caused_by_trigger_id, caused_by_trigger_kind)` consistent with its `execution_origin`: `manual ↔ (null, "manual")`, `scheduled ↔ (schedule_id, "schedule")`, `event ↔ (trigger_id, "event")`.

**Explicit non-properties:**

- **No hop-bound proof.** Cycles are operator-observable and bounded by existing L1/admission proofs only. Documented in the Lean spec as an intentional omission.
- **No strict-serial queue proof.** `Serial = skip` is v1 semantics; we don't claim "every matching doc fires exactly once."

**Conformance tests.** `tests/state_machine_conformance.rs` gains serial-skip, latest_only-supersede, and template-render-failure cases. New files `tests/trigger_conformance.rs` (event path) and `tests/schedule_conformance.rs` (cron path under the new split) cover end-to-end fire behavior. The existing `lifecycle_regression.rs` picks up new `Superseded` sources from `latest_only`.

## Rollout

Three PRs landing in order. Each is independently shippable and reviewable.

### PR 1 — Task/Schedule split + TriggerEngine scaffold

- New schemas: `Task`, `Schedule`. Remove `ScheduledTask` schema outright.
- New fields on `AgentRequest`: `caused_by_trigger_id`, `caused_by_trigger_kind`.
- Build `TriggerEngine` + `TriggerSource` trait + `ScheduleSource`.
- Retarget today's `Scheduler::run()` call site to `TriggerEngine::run(vec![ScheduleSource::new(...)])`. Cron behavior is unchanged from the outside.
- Lean: extend `Scheduling.lean`; prove T1, T2, T4 for the schedule path.
- Conformance: `tests/schedule_conformance.rs`.
- Desktop UI retargeted to show `Task` + `Schedule` instead of `ScheduledTask`.

No migration. Operators re-create their schedules through the apply flow (clean break; existing `ScheduledTask` apply manifests are rewritten to `Task` + `Schedule` pairs by hand — mechanical mapping).

### PR 2 — EventTrigger + EventSource

- New schema: `EventTrigger`.
- Add `EventSource` implementing `TriggerSource`; engine registers it alongside `ScheduleSource`.
- MiniJinja dependency + template module (used by both cron and event paths; cron gets it in PR 2 as a drop-in replacement for today's raw-string prompt).
- Pre-validation during apply for `Task` templates (against all trigger scopes pointing at the task).
- Lean: `Triggers.lean`; prove T1–T4 for the event path.
- Conformance: `tests/trigger_conformance.rs`.

### PR 3 — Manual runs + operator ergonomics

- `ManualSource` + `run_task_now(task_id, args)` helper wired to CLI and desktop.
- Desktop observability polish: `last_fired_at`, `fire_count`, `last_error` surfaced on Task detail views; lineage badges on requests showing their trigger origin.
- CLAUDE.md updates covering the trigger model; apply-manifest doc updates.

## Testing strategy

**Unit tests (colocated `<module>/tests.rs`):**

- Template rendering: scope isolation (schedule templates can't reference `doc.*`), strict-undefined behavior, size caps.
- Filter pre-validation against GraphQL schemas.
- Concurrency-mode dispatch decisions with mocked in-flight queries.
- Lineage field population in the materializer.

**Conformance tests (`crates/defra-agent/tests/`):**

- `schedule_conformance.rs` (PR 1): fire at `next_run_at`, enabled gate, template render failure records `last_error`, serial skip under in-flight, `latest_only` supersedes.
- `trigger_conformance.rs` (PR 2): filter match/miss, enabled gate, template failure, backfill is forward-only (seed docs before trigger exists, enable trigger, write new doc, verify only the new doc fires).

**Soak test (extend existing infrastructure):** K schedules + M event triggers pointing at L tasks; generate load by writing matching docs at rate R; run for N minutes; assert `fire_count` monotone, no stuck non-terminal requests, subscription drops compensated by the bounded-poll fallback.

## Risk register

| Risk | Mitigation |
|---|---|
| MiniJinja binary-size or footprint too high | Confirm during PR 2; fall back to Tera or a minimal substituter if unacceptable |
| Subscription refresh storms when many triggers land at once | Debounce refresh in `EventSource` (coalesce within ~250ms) |
| Operator forgets to rewrite apply manifests from `ScheduledTask` | Detect during apply; emit a specific error pointing at the field-mapping guide |
| Template validation against large collection schemas is slow | Cache parsed schemas in the engine; invalidate on schema reload |
| Runaway cycle consumes inference budget | Existing admission caps bound spend-per-unit-time; operator disables the trigger |

## Out of scope (explicit)

- **Update / delete event kinds.** `event_kind` field reserved; v1 ships `"created"` only.
- **Hop counter / request-write provenance.** Deferred indefinitely pending concrete operational need.
- **Strict-serial queuing.** `Serial = skip` is v1 semantics.
- **Task-specific output schemas.** `output_schema_ref` field reserved; the typed-pipeline story lands separately.
- **`ScheduledTask` migration tooling.** Clean break; manifests are rewritten by hand.
- **Full `AgentPrincipal`/`AgentBehavior` runtime split (issue #9).** `Task.behavior_id` is the only identity reference; the rest of the runtime's identity refactor is orthogonal to this spec.
