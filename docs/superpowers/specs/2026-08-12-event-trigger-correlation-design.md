# Event-trigger correlation and fan-in (design)

Issue: #1096. Predecessors: `2026-08-07-event-trigger-graph-experiments-design.md`
(document-pipeline DAGs), `2026-08-07-datastore-tool-surface-design.md`
(reusable create-tool grants).

## Problem

EventTrigger graphs have no notion of a **run**. The concurrency gate resolves
against `(agent_did, trigger_id, trigger_kind)` — unique per agent since #605,
but not unique per traversal of the graph. Two consequences:

1. **The gate leaks across runs.** `serial` drops a second run's fires
   permanently (nothing requeues them); `latest_only` cancels a first run's
   in-flight work. A graph is only safe to run concurrently if every trigger in
   it is `parallel`, forfeiting the other two modes.
2. **Fan-in is unexpressible.** No predicate spans a group of documents, so a
   barrier must be hand-rolled out of denormalized counts, sentinel documents,
   a counting *gate behavior* burning one inference call per sentinel to
   compute a `COUNT(*)`, and a cron backstop — and it still is not correct,
   because no concurrency mode is safe for that gate.

Both reduce to one missing primitive: **a correlation tag that propagates
through the graph**.

## Invariant

A trigger may declare a correlation field. Fires, the concurrency gate, and
completion predicates are evaluated **within** the key
`(target_agent_did, trigger_id, trigger_kind, correlation)`, never across keys.
A `per_group` trigger materializes at most one request for a well-formed,
closed group, with the complete projected membership in template scope, and no
agent participates in deciding whether the group is complete. Under fair
rescans and successful store operations, a durable eligible group eventually
materializes that request.

"Complete" necessarily relies on a producer contract: the producer declares
the final cardinality and creates exactly that many matching documents. A
barrier cannot know that a producer will not add a late member after declaring
the set complete. The runtime detects an already-overfull group and fails
closed, but it cannot retract a request if a producer violates the contract
after the request has been created. Correlation, expected-count, and
filter-relevant source fields must also remain stable, and source rows must
remain durable until the group resolves. Pack schemas mark the correlation and
expected-count fields `@immutable`; arbitrary external schemas remain subject
to this documented producer obligation.

## Decision

Propagate a tag; derive membership and resolution from documents that already
exist.

Membership and successful-fire markers are queries over source documents and
`AgentRequest` rows. One internal `EventTriggerGroupState` collection persists
the first-seen clock for timeout groups; it is not a membership snapshot or a
fired marker. Values needed by runtime-filled write-tool fields are snapshotted
on the request as immutable trigger execution context; that is per-request
lineage, not mutable group state.

## What already exists

Establishing the baseline, because the design is mostly *composition*:

| Need | Existing mechanism |
| --- | --- |
| Conditional routing (one doc, N disjoint outcomes) | N `EventTrigger`s with disjoint `filter` fragments |
| Fan-out | multiple triggers matching one create |
| Stage edges | `DatastoreToolSurface` → `BoundedWriteTool` → create in a watched collection |
| Reading a whole group | `defra_query` + `defra_query_collections` allowlist |
| "Has this already fired?" | `AgentRequest` query on trigger lineage — the gate already has the query seam |
| Whole source doc at fire time | `fetch_source_doc` hydrates `doc_vars` **before** dispatch (`event_source.rs:618`) |
| Request → source lineage | `caused_by_source_doc_id`, stamped and `@immutable` |
| Per-request ambient context for tools | `tokio::task_local!` `TOOL_RUNTIME_SCOPE` (`tool_call_lifecycle/runtime.rs:183`), read by `call_tool_managed` |
| Timer cadence | `rescan_tick`, already firing every 5s (`event_source.rs:145`) |

Two facts that shaped the design, both verified against the pinned
`defradb.rs` (`c3f5168`):

- **SDL directives are a closed allowlist** (`KNOWN_FIELD_DIRECTIVES` /
  `KNOWN_TYPE_DIRECTIVES`, `query-parse/src/sdl_parse/directives.rs`). A custom
  `@agentTool`-style annotation would be rejected at schema registration. Tool
  grants stay in document config, where they already are.
- **`build_tools` runs once per behavior at startup** (`agent/runtime/context.rs:116`,
  inside `run_behavior`), and `loop_tools` is an `Arc` shared across every
  request that behavior serves. A correlation value therefore **cannot** be
  baked into a `BoundedWriteTool` at construction; it must reach the tool at
  call time, via the existing task-local.

## Architecture: the tag spine

The propagation path crosses the durable request boundary. That boundary is
load-bearing: a task-local created in the event source cannot survive until a
watcher later claims and runs the request.

```text
source doc[correlation_field + selected source-fill fields]
  │  (1) already in doc_vars — zero extra queries
  ▼
FireIntent.correlation + group_vars + trigger_context
  │  (2) trigger_engine/mod.rs
  ▼
TriggerLockKey + gate filter
  │  (3) trigger_engine/mod.rs   (4) production_materializer.rs
  ▼
AgentRequest.caused_by_correlation + caused_by_trigger_context
  │  (5) lifecycle::TriggerLineage / materialize.rs
  ▼
watcher::AgentRequest
  │  (6) watcher load + derived-request inheritance
  ▼
ToolRuntimeScope.correlation + source_fields
  │  (7) daemon scope; preserved by nested/background scopes
  ▼
BoundedWriteTool stamps declared fill fields
  │  (8) defra_write/mod.rs
  ▼
next trigger reads it off the source doc      (9) loop closed
```

| # | File | Change |
| --- | --- | --- |
| 1 | `trigger_engine/event_source.rs:618` | read `doc_vars[correlation_field]` |
| 2 | `trigger_engine/mod.rs:46` | add correlation, optional `group_vars`, and the snapshotted source-fill context to `FireIntent`; only `group_vars: Some` enables group-marker dedupe |
| 3 | `trigger_engine/mod.rs:23` | `TriggerLockKey` → `(target_agent_did, trigger_id, TriggerKind, Option<correlation>)`; use weak/pruned lock entries so run ids do not leak memory |
| 4 | `trigger_engine/production_materializer.rs:171,234` | gate methods take `correlation: Option<&str>`; filters gain `caused_by_correlation: { _eq: … }` **only when `Some`** |
| 5 | `lifecycle::TriggerLineage` | carries correlation and versioned trigger context; both are written to the request |
| 6 | watcher + subagent/internal request materializers | load the immutable lineage into the claimed request; any request explicitly derived from a parent copies correlation/context |
| 7 | `agent/daemon/inference.rs`, `tool_call_lifecycle/runtime.rs`, background bridge | install and preserve correlation/source fields in every tool execution scope |
| 8 | `defra_write/mod.rs` | `build_mutation` stamps fill fields from `current_tool_runtime_context()` |
| 9 | — | closes at hop 1 for the next trigger |

**Configuration compatibility remains, storage compatibility does not.** An
`EventTrigger` with no `correlation_field` yields `None` at every hop; the gate
filter omits its clause and dispatch behavior is unchanged. However, this is an
explicit breaking schema cut: existing homes are not upgraded in place.

Update the canonical `AgentRequest` and `EventTrigger` SDL, refresh their
frozen migration-baseline SDL/version pins, and require operators/developers to
reinitialize stores. Do not add `PatchVersioned`, `PatchInPlace`, lenses, or
backfill code for this feature. Startup against a pre-cut store must fail with
a clear schema-version/reset instruction rather than partially running against
missing columns. This removes compatibility migration from every PR slice; it
does not weaken the runtime semantics for newly created rows.

## Surface

### `EventTrigger` (existing collection, new columns)

| Field | Meaning |
| --- | --- |
| `correlation_field` | Field on the source doc identifying the run. Scopes the gate and groups fires. |
| `fire_mode` | `per_document` (default, today's behavior) or `per_group` |
| `expected_count` | Literal group width — fixed-width fan-out (3 verifiers, 5 judges) |
| `expected_count_field` | Field on the source doc carrying the width — producer-decided |
| `group_timeout_secs` | Fire with the partial group this long after the group's first-seen |
| `group_min_count` | Floor for a timeout fire; below it, keep waiting without firing |

A `per_group` trigger uses either exactly one of `expected_count` and
`expected_count_field`, or neither when `group_timeout_secs` is set. The latter
is a pure-timeout barrier: it never completes early and fires the accumulated
group with `group.complete == false` when the timeout and minimum are met.
Count fields are not legal under `per_document`. `expected_count_from` is
deliberately deferred: its correctness depends on an upstream closed-set
signal that the current surface does not have, so documenting a producer race
does not make it a safe primitive to ship.

### `AgentRequest` (existing collection, durable lineage)

```graphql
caused_by_correlation: String @index @immutable
caused_by_trigger_context: String @immutable
```

`caused_by_correlation` follows the `caused_by_source_doc_id` precedent:
lineage, indexed, immutable. The existing `caused_by_trigger_id` and
`caused_by_trigger_kind` fields also become immutable because the request row
cannot be a durable group marker if any part of its key can be rewritten.

`caused_by_trigger_context` is versioned JSON containing only source fields
referenced by `Fill::SourceField` in the resolved tool surface at
materialization time:

```json
{ "version": 1, "source_fields": { "expected_total": "5" } }
```

It is not indexed and it does not contain the whole source document. Snapshot
only strings and integral JSON numbers (normalized to canonical strings), cap
the encoded payload at `MAX_TRIGGER_CONTEXT_BYTES = 16 KiB`, and fail
materialization if a declared source field is missing, has any other type, or
causes the payload to exceed the cap. This matches the existing bounded-write
tool's string-valued contract and makes tool-time fill independent of later
source-document mutation, deletion, or config changes.

Any subagent, goal continuation, background-completion wakeup, or other
internal request that is explicitly derived from a parent request inherits
both fields unchanged. It may stamp its own immediate
`caused_by_trigger_id/kind` while retaining the graph-run correlation. This
makes correlation useful across mixed document/subagent execution and prevents
a child write tool from losing the runtime fill context. Request creation paths
with no validated parent do not populate these fields from tool arguments or
other caller-supplied runtime input.

### `WriteToolField` (no schema change)

`write_tools` and `DatastoreToolSurface.entries` are `[String]` columns of JSON,
so a new optional key on the declaration needs no additional collection field.

```json
{ "name": "run_id",         "fill": "correlation" }
{ "name": "expected_total", "fill": { "source_field": "expected_total" } }
```

The accepted JSON grammar is exactly `null`/absent, the string
`"correlation"`, or an object with the single key `"source_field"` whose value
passes the GraphQL field-identifier validator. Unknown strings, keys, or mixed
objects are rejected at deserialization/validation.

A filled field is:

- **omitted from the model-visible JSON schema** in `BoundedWriteTool::definition()`
- **rejected** if the model passes it anyway
- filled by the runtime at `call()` time

So the tag cannot be forged, and cannot be forgotten. This is the property that
makes graphs composable: correlation is a runtime invariant, not prompt
discipline.

`Fill::SourceField` copies a named string/integer field from the *triggering
source document snapshot* through the agent hop, using the canonical string
representation expected by today's bounded-write tool. It exists because the
alternative — instructing a model to retype `expected_total` on every write —
reintroduces exactly the fragility the tag removes, for exactly the value a
barrier depends on. For `per_group`, "triggering source document" means the
deterministic representative document defined below.

### Template scope

Beside today's `{{ doc.* }}` / `{{ event.* }}` / `{{ node.* }}` / `{{ ctx.* }}`:

- `{{ event.correlation }}` — available in both fire modes
- `{{ group.correlation_value }}`, `{{ group.count }}`, `{{ group.docs }}`
  (projected source docs), `{{ group.complete }}` (`false` on a timeout fire)
  — `per_group` only

`group.docs` is ordered by `_docID` ascending. In `per_group` mode, `doc` and
`event.source_doc_id` refer to the first document in that order. This gives
timeout fires and restart recovery the same deterministic singular lineage as
completion fires; no "last arriving document" exists on a timer tick.

Internally, `FireIntent.group_vars: Option<Value>` and
`TemplateScope.group: Option<Value>` keep this distinction explicit. A
correlated `per_document` intent has a correlation value but no group scope;
it must not be suppressed by the successful-fire marker for an earlier member
of the same run.

## Fire semantics

### Completion is evaluated in the event source, not in `dispatch`

An incomplete group emits **no `FireIntent` at all**. Routing it through
`dispatch` to be `Skipped` would make every non-final member write
`last_status: "skipped"` and bump the trigger's runtime fields, so `fire_count`
and `last_error` would stop meaning anything operationally.

The existing `seen_docs` remains the collection/document fast path once every
matching trigger has settled and retains its forward-only meaning. While one
trigger is waiting for correlation, the source also tracks settled
`(trigger_id, source_doc_id)` delivery identities in memory. Ready siblings
fire and become individually seen; only the incomplete sibling remains
eligible for a follow-up update. Once all siblings settle, the document moves
to the ordinary collection-level seen set and the partial entry is removed.
Group state is separate and keyed by `(trigger_id, correlation)`: filters,
expected cardinality, timeouts, and task bindings belong to a trigger, not
merely to a collection.

Group reconciliation has three paths:

1. **Startup/config activation:** admit at most one deterministic recovery page
   across triggers whose membership fingerprint (`source_collection`,
   `filter`, `correlation_field`) is new or changed. Unrelated snapshot
   generation bumps perform no group-recovery query. The ordinary seed still
   suppresses old documents for `per_document`; the rotating sweep discovers
   the remaining pre-existing groups. Complete groups are eligible immediately
   and incomplete groups load or create their durable first-seen clocks.
2. **Steady-state subscription:** hydrate the new source document, identify its
   trigger/correlation key, and reconcile only that dirty group.
3. **Loss repair:** the existing collection-level `rescan_tick` discovers new
   document ids. It marks only correlations containing newly discovered rows
   dirty. A bounded rotating recovery sweep also revisits old pages so a
   failed startup page or dropped bookkeeping cannot strand a group.

`SEEN_DOCS_SEED_LIMIT` is not a group-recovery bound. Recovery pagination uses
a stable ordering and evaluates a group only from a complete membership
snapshot. For each bounded batch of candidate correlations, load request
markers with one `_in` query and discard already-marked groups before hydrating
their full documents; do not load all historical markers into an unbounded
process cache and do not issue one marker query per historical group.

This makes the steady-state processing cost proportional to new/dirty groups,
a fair batch of at most 16 due cached timers, and one rotating recovery page.
Both sequential group loops poll cancellation between membership queries. The existing rescan still
reads collection document ids and therefore retains its current
linear-in-collection-size I/O cost, but fan-in does not multiply that into a
full collection scan per trigger or generation bump. A complete rotating cycle
remains linear in historical matching rows; large installations need source
retention/archival. This implementation targets the existing non-catalog
event-source scale and records page, row, dirty-group, marker-prune, and
sweep-duration metrics.

Group membership is the set of source documents that satisfy **both** the
trigger's existing `filter` and
`correlation_field == correlation_value`. Counting every same-tag row while
projecting only filter-matching rows would allow a disjoint route to complete
the wrong barrier.

### The request row is the persisted resolution marker

Before a group fires, `dispatch` queries whether an `AgentRequest` exists with the full key
`(target agent_did, caused_by_trigger_id, caused_by_trigger_kind = "event",
caused_by_correlation)` in **any** lifecycle state. Non-empty means the group
has already materialized and must not fire again. Correlation values are
required to be non-empty strings and are escaped with
`graphql::escape_graphql_string()` at every query site.

This marker query runs only for an intent with `group_vars: Some`; ordinary
correlated `per_document` intents continue to materialize once per matching
source document.

This answers the issue's resolution-durability question without a separate
fired marker: a node restart can neither double-materialize a group that
already has a request nor lose an eligible group, because the marker is the
durable request row and membership is rebuilt by the explicit group reconciler.
`EventTriggerGroupState` affects timeout liveness and recovery cost only; it is
never consulted for the already-fired decision.

The marker check and request creation occur under the same tag-keyed process
lock for **all** concurrency modes, including `parallel`; `serial`'s active-row
check and `latest_only`'s supersession also run inside that lock. Lock-map
entries use weak references or are pruned after use so unique run ids do not
grow the map forever.

This proves process-local at-most-once under the runtime's single-writer
deployment invariant: each `(did, behavior)` is active on one deployment. The
query/create pair is not a database transaction and therefore is not a
multi-writer uniqueness mechanism. Supporting overlapping deployment handoff
or active-active execution would require a unique durable group-claim key and
is out of scope. Liveness also assumes fair rescans and eventually successful
queries/materialization. State these assumptions in the Lean model rather than
calling the request-existence query alone an unconditional exactly-once proof.

### Counting

Use a bounded `_docID` + expected-field membership projection with the combined
filter above. The same rows later hydrate `group.docs`, so correctness does not
depend on DefraDB aggregate syntax. Introduce
`MAX_EVENT_TRIGGER_GROUP_DOCS = 256`; reject literal counts above it and fail a
dynamic group closed if its expected or actual count exceeds it. This bounds
query memory and prompt construction independently of the 1 MiB rendered
template cap.

`expected_count` must be in `1..=MAX_EVENT_TRIGGER_GROUP_DOCS`.
`expected_count_field` must be present on every member, be either a JSON
integer or canonical decimal string, resolve to the same positive value on
every member, and fall within the same bound. A missing, malformed, zero,
negative, or inconsistent value is an operational error and emits no
`FireIntent`.

The group is complete only when `actual_count == expected_count`. An
`actual_count > expected_count` group fails closed as a producer-contract
violation; it does not choose an arbitrary subset. Once a request marker
exists, any later matching source document is likewise logged as a late-member
contract violation and suppressed.

### Timeout

Rides the existing 5s `rescan_tick`. The runtime loads or creates one immutable
`EventTriggerGroupState.first_seen_at` row keyed by a digest of trigger id,
membership fingerprint, and correlation. The in-memory timer is only a bounded
cache, so restart and cache pressure preserve the timeout deadline. Changing
the source collection, filter, or correlation field creates a fresh clock;
task and count-policy changes retain the original first-seen instant.

`group_min_count` gates the timeout fire and defaults to 1. Below the floor the
runtime does not create a permanent "expired" tombstone; it keeps the group
eligible and fires on the first later reconciliation at which the timeout has
elapsed and the floor is met. Permanent silent expiry would require a durable
resolution marker and is not promised by this design.

After a successful materialization (or discovery of an existing marker), evict
the group's in-memory timeout entry. Query or materialization failures retain
the cache entry for retry. Removing or disabling a trigger evicts cache entries;
re-enabling it reloads the durable clock.

Timeout working state is bounded by
`MAX_ACTIVE_EVENT_TRIGGER_GROUPS_PER_TRIGGER = 4096`. When a timeout elapses
below `group_min_count`, mark the cached timer dormant so due-timer processing
does not query it repeatedly. Dormant entries are retained in a bounded LRU
cache capped by
`MAX_DORMANT_EVENT_TRIGGER_GROUPS_PER_TRIGGER = 4096`. It is not queried on
every tick. A newly discovered member reactivates and reconciles the group
immediately. When the active cache is full, new correlations remain
discoverable by the rotating sweep and consult their durable clock without
entering the cache; capacity pressure can add database reads but cannot strand
an eligible timeout fire.

Permanently malformed groups (inconsistent or invalid cardinality, overfull,
or over the hard cap) set `quiesced_at` and a diagnostic reason on this same
state row. Due-timer processing then stops considering them, and each bounded
recovery page batch-loads request markers and quiescence markers before it
loads group membership. A membership-definition change produces a new state
key and is therefore the explicit way to reconsider a quiesced group.

State rows are retained in this release, so durable storage grows by one small
row per distinct observed correlation. The runtime performs indexed key or
bounded `_in` lookups only; it never full-scans the state collection. Retention
or compaction of historical rows is follow-up operational work.

### Concurrency gate, with the tag

- `serial` — trigger-wide for `per_document`, matching the Lean concurrency
  model even when correlation is carried for lineage/fills; per `(trigger,
  run)` for `per_group`.
- `latest_only` — supersedes trigger-wide for `per_document`; supersedes only
  within the run for `per_group`.
- `parallel` — requests still execute in parallel; only the per-key group
  marker decision/materialization critical section is serialized.

The existing expired-claim carve-out (`row_gates_serial_fire`,
`production_materializer.rs:87`) is unaffected and composes: a past-deadline
orphan still does not gate, scoped according to the fire mode above.

## Validation

`desired_state/validate.rs` (~line 698), plus the runtime backstop at snapshot
build:

- `fire_mode: per_group` without `correlation_field` → reject
- `per_group` without a count source and without `group_timeout_secs` → reject
- both `expected_count` and `expected_count_field` → reject
- any `expected_count*` under `per_document` → reject
- `group_timeout_secs` / `group_min_count` under `per_document` → reject
- `group_min_count` without `group_timeout_secs` → reject
- zero-valued counts/timeouts, a literal expected count above
  `MAX_EVENT_TRIGGER_GROUP_DOCS`, or a literal `group_min_count` above
  `expected_count` → reject
- `correlation_field` must pass a field-identifier check — it is interpolated
  into filter fragments, so it gets the same defense class as
  `validate_collection_identifier` (see the GraphQL sharp edge in `CLAUDE.md`)
- a `WriteToolField` with any `fill` must not be `required` — the model cannot
  supply it
- a `Fill::SourceField` name must pass the GraphQL field-identifier validator
- a schedule or `per_document` event trigger whose task template references
  `group.*` → reject; only a `per_group` event trigger provides that scope
- live validate: `correlation_field` exists on the source collection and is a
  string field; `expected_count_field`, when configured, exists and is a
  string or integer field
- snapshot build repeats the safety checks and quarantines invalid triggers;
  CLI validation is not a trusted runtime boundary

### Fail-closed, both directions

- **Tag missing on the source doc** while `correlation_field` is declared →
  defer only that trigger's delivery identity without consuming it. Other
  matching triggers on the same physical document settle independently. A
  create-then-populate producer can supply the tag in a follow-up update,
  which is then handled as that trigger's created delivery. Firing untagged
  would silently rejoin unrelated runs inside the gate.
- **Tag empty or not a string** → defer the same way as a missing tag.
- **Expected count missing, malformed, inconsistent, overfull, or over the
  cap** → emit no intent and durably quiesce the correlation with a diagnostic
  reason.
- **`fill: correlation` with no ambient tag** (e.g. someone manual-runs the
  behavior) → the write fails with an error naming the trigger, rather than
  quietly creating a document orphaned from every group.
- **`fill: {source_field: ...}` with no immutable source snapshot** → the write
  fails; it never queries a mutable source document at tool-call time.

### Self-config

`fill` is a `write_tools` / surface-entry concern, and `write_tools` is already
in the protected, never-self-patchable set (`self_config/mod.rs`). No new
self-config category. An agent must not be able to grant itself a tool that
stamps a tag of its choosing.

## Proofs

Per the foundation flow, the Lean definitions and theorem statements **are**
the specification of the fix and land first.

- `Proofs/Triggers/Types.lean`, `Serial.lean`, `LatestOnly.lean` retain the
  request-lifecycle model's trigger-wide `(trigger_id, kind)` abstraction.
  `Proofs/Triggers/Groups.lean` models the production gate identity explicitly:
  target DID + trigger id + kind for `per_document`, with correlation added for
  `per_group`. The existing lifecycle preservation theorems therefore remain
  trigger-wide; the group model proves that different correlations share that
  scope only in `per_document` mode. Rust dispatch and embedded-node materializer
  tests are the conformance fence for selecting and persisting those scopes.
- `Proofs/Triggers/Lineage.lean` and `Proofs/DurableLineage.lean` — the tag
  joins stamped lineage and parent-derived requests preserve correlation while
  retaining their own immediate trigger cause.
- New `Proofs/TriggerGroups` model and conformance traces:
  - membership uses `(filter AND correlation)`, not correlation alone;
  - different correlation keys do not interfere;
  - marker existence implies no second materialization under the
    single-writer/locked transition relation;
  - an eligible durable group with no marker converges to one marker under a
    fair rescan/materialization assumption;
  - restart discards clocks and volatile seen state but preserves source rows
    and request markers, so completed groups remain suppressed and eligible
    unmarked groups remain discoverable;
  - overfull/inconsistent groups fail closed.
- Keep `EventSource`'s existing `EventDeliverySourceContract`
  `dedupe_policy: "monotone_once"`: it still describes per-document delivery.
  Group reconciliation is a separate durable projection and should not be
  hidden by changing the meaning of that existing contract string.

## Testing

Conformance tests driven from the spec change, plus e2e beside
`crates/gents/tests/e2e_triggers/event_trigger_e2e.rs`. The load-bearing cases:

- Two runs interleaved through one `serial` trigger: no skip and no
  supersession attributable to a different correlation value.
- `correlation_field` + a count source: materializes exactly one request per
  valid closed group, after the last member create, with the deterministically
  ordered full group in scope.
- The same correlation on two different triggers does not merge membership;
  trigger filters exclude non-members from both count and `group.docs`.
- Two correlated `per_document` members still materialize two requests; group
  marker dedupe is never inferred from correlation alone.
- The same trigger/correlation under a different target DID neither gates nor
  satisfies the marker lookup.
- A group that never completes: one fire at `group_timeout_secs` with
  `group.complete == false` once `group_min_count` is met; no fire while below
  the floor.
- A pure-timeout group with no count source never fires early.
- Thousands of abandoned below-floor groups leave the active timer set after
  timeout; steady-state ticks do not query each dormant group, memory remains
  within the configured constants, and a new member reactivates its group.
- Startup recovery and dropped-event repair are paginated and eventually visit
  every group; a normal tick does not perform one full collection scan per
  `per_group` trigger.
- Restart mid-group: neither double-materializes a group with a request marker
  nor loses a durable eligible group; the incomplete group's durable timeout
  clock retains its original first-seen instant.
- Crash before request creation is retried; crash after request creation is
  suppressed by the marker.
- Inconsistent expected fields, overfull groups, and group sizes over the cap
  fail closed and are durably quiesced; missing/empty tags defer until a
  follow-up update supplies a usable correlation.
- A ready trigger and a correlation-incomplete trigger observing the same
  physical document are independent delivery identities: the ready trigger
  fires once immediately, the incomplete sibling fires once after a follow-up
  update, and neither is duplicated. Startup seeding preserves the same
  forward-only behavior for pre-existing shared documents.
- Triggers without the new fields retain today's dispatch semantics.
- Foreground and background write-tool calls both preserve correlation and
  source-field fills across the durable request boundary.
- Subagent/internal child requests inherit immutable correlation/context from
  their parent, while runtime-created manual roots leave both fields empty.
- Template validation rejects `group.*` outside a `per_group` event trigger,
  and every existing `TemplateScope` construction explicitly supplies
  `group: None`.
- `config validate` rejects each invalid combination above.
- A fresh store registers the new canonical `AgentRequest`/`EventTrigger`
  baseline and pins; a pre-cut store fails with the documented reinitialize
  instruction. There is no migration/backfill test matrix.

Gate with the full `cargo test -p gents` suite and
`cargo check --workspace --all-targets`, per `CLAUDE.md`.

## Acceptance: `packs/code_review`

A self-contained pack with real (non-toy) stages that reviews **this
repository** by default, following the `packs/pipeline` layout precedent.

```text
ReviewJob (seed)       ──► recon    ──► 4 × ReviewArea
ReviewArea             ──► scan     ──► CandidateFinding*, 4 × ScanResult
ScanResult (fan-in)    ──► verify   ──► FindingVerdict*, VerificationSummary
VerificationSummary   ──► triage   ──► confirmed Finding*, TriageReport
```

- **recon** runs deterministic Rust pre-scan commands, chooses exactly four
  distinct review lenses, and stamps `expected_total: 4` on every
  `ReviewArea`. It uses the coordinator model and the schema-generated
  `write_review_area` tool.
- **scan** fires once per area on the reviewer model. Its bounded evidence
  packet contains the lens-specific source context, so the four parallel
  scanners need only `write_candidate_finding` and `write_scan_result`. A
  scanner writes at most one strongest evidenced Critical/Major candidate and
  exactly one sentinel. Both tools stamp `run_id` with `fill: correlation`;
  the sentinel also stamps `expected_total` with
  `fill: {source_field: "expected_total"}`.
- **verify** is `per_group` on `ScanResult` with `expected_count_field:
  "expected_total"`, `concurrency: serial`, and `correlation_field: "run_id"`.
  It fires once with deterministic `group.docs`, rereads every candidate and
  the source, then writes exactly one `FindingVerdict` per candidate plus one
  count-balanced `VerificationSummary`. Its prompt names the generated
  `defra_query`, `write_finding_verdict`, and `write_verification_summary`
  tools exactly.
- **triage** fires once from `VerificationSummary`, reads the closed verdict
  ledger, promotes only confirmed rows through `write_finding`, and writes one
  `TriageReport` through `write_triage_report`.

The `ScanResult` sentinel stays — one create by an agent that already has a
write tool is a legitimate "worker finished" signal. What the feature removes
is the **gate behavior** (one inference call per sentinel to compute a
`COUNT(*)`, a model deciding a numeric comparison) and the **cron backstop**
(replaced by `group_timeout_secs`). That is the issue's actual complaint, and
the claim should not be stated more broadly than that.

Recon, verification, and triage receive read-only file tools and a
network-enabled shell rooted at `GENTS_REVIEW_ROOT`; scanners receive only the
bounded evidence packet and their two write tools. DefraDB query/write access
remains stage-specific, and the pack declares a `write` principal ceiling.

Because the pack runs against this repo, it is tunable in place: real findings
on real code are a useful operator signal, not a deterministic test oracle.
Automated acceptance checks graph structure and durable outputs: four tagged
`ReviewArea` rows with one consistent expected count, four distinct completed
scan requests and sentinels, one correlated verification request, a one-to-one
candidate/verdict ledger, one count-balanced verification summary, one final
triage request, one report, and signed tool-call provenance for all seven
requests. It also validates every prompt against the exact generated tool names
and query collection allow-list. Finding quality remains a smoke/evaluation
criterion because model output is not deterministic.

Related, not blocking: `sourcenetwork/defending-code-reference-harness` runs
the same shape (recon → find → grade → judge), and its judge stage is
documented as running serially "so that two duplicate findings arriving around
the same time aren't accidentally both classified as new" — the filed bug in
its natural habitat. Adopting this pack shape there is follow-up work.

## Non-goals

- **New event kinds.** `event_kind` stays `created`-only. Updates, transitions,
  and lifecycle-completion edges are separate work. The six-role autonomous
  turn loop (SEM/NEP/EG/BT/PI/RFI) that motivated this scoping is fully
  expressible create-only: state fusion creates the next state doc, gate
  verdicts route through disjoint filters on a created verdict doc, and
  *terminate* is simply not creating the next document — termination is the
  absence of an edge and needs no mechanism.
- **Update / delete write tools.** Not required by anything above.
- **Cycle bounds.** Cycles are already expressible today and already unbounded;
  making them cheap to bound is deliberately out of scope here.
- **A `TriggerRun` collection.** Everything it would hold is derivable from
  tagged `AgentRequest` rows.
- **Permanent-expiry state.** See the timeout/min-count semantics above. Source
  membership, first-seen clocks, and successful resolution are durable; a
  below-floor timeout does not permanently close the group.
- **Migration of pre-feature stores.** This is an accepted breaking schema cut;
  existing homes must be reinitialized. No patch step, lens, or data backfill
  ships with the feature.
- **`{{ args.* }}` in event-trigger scope.** Unchanged from the predecessor
  design.

## Slicing

Because this is a breaking fresh-store cut, the schema/runtime work lands as
one PR. Splitting its columns across independently released PRs would require
multiple resets and would ship stored-but-unused fields. Within that PR, keep
the foundation order as reviewable commits:

| PR / commit | Contents |
| --- | --- |
| **Runtime PR, commit 1** | Expand the Lean trigger key; add `Proofs/TriggerGroups`, durable-lineage obligations, and generated conformance cases. |
| **Runtime PR, commit 2** | Update all canonical `AgentRequest` / `EventTrigger` SDL fields, add the internal `EventTriggerGroupState` clock collection, and refresh frozen baseline pins. Add no migration step, lens, or backfill. |
| **Runtime PR, commit 3** | Implement correlation through materialization, fire-mode-scoped gate/marker queries, bounded locks, timeline/protocol/CLI projections, immutable trigger context, parent-derived inheritance, runtime scopes, and write-tool fills. |
| **Runtime PR, commit 4** | Implement `per_group`: combined-filter membership, startup/rotating recovery cursors, dirty-group fast path, batched marker pruning, bounded active/dormant tracking, count and timeout-only modes, deterministic scope, validation, metrics, restart/retry behavior, and e2e tests. Keep the existing EventDelivery contract per-document. |
| **Demo PR** | `packs/code_review` pack + deterministic graph acceptance checks + operator README. |

The runtime PR is mergeable only as a complete unit: no released intermediate
contains a stored-but-unused field or a schema/runtime mismatch.

## Success criteria

- Two runs interleaved through the same graph never interfere.
- A valid closed `per_group` input materializes at most one request with the
  full, deterministically ordered group in scope; under fair successful
  rescans it materializes one, and no agent computes a count.
- A group that never completes fires once at timeout with
  `group.complete == false` after meeting `group_min_count`, and does not fire
  while below the floor.
- Abandoned below-floor groups do not cause unbounded active timers or
  per-tick queries; later membership can reactivate them.
- A restart mid-group neither duplicates a materialized group nor loses a
  durable eligible group or restarts its timeout deadline.
- On a fresh post-cut store, triggers without the new fields behave exactly as
  today.
- `config validate` rejects every invalid field combination listed above.
- `packs/code_review` proves N correlated area/result rows and one correlated
  triage request/report, with a barrier stage that is a plain agent containing
  no counting logic. Real finding quality is evaluated separately.
