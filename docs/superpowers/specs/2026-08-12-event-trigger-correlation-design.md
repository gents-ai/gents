# Event-trigger correlation: run-scoped gates and fan-in (design)

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
completion predicates are evaluated **within** a correlation group, never
across groups. A trigger with a completion predicate fires exactly once per
group, with the whole group in template scope, and no agent participates in
deciding whether the group is complete.

## Decision

Propagate a tag; derive everything else from documents that already exist.

**Zero new collections.** Group state, fired-markers, and turn counts are not
persisted — they are queries over `AgentRequest` rows that the runtime already
writes and indexes.

## What already exists

Establishing the baseline, because the design is mostly *composition*:

| Need | Existing mechanism |
| --- | --- |
| Conditional routing (one doc, N disjoint outcomes) | N `EventTrigger`s with disjoint `filter` fragments |
| Fan-out | multiple triggers matching one create |
| Stage edges | `DatastoreToolSurface` → `BoundedWriteTool` → create in a watched collection |
| Reading a whole group | `defra_query` + `defra_query_collections` allowlist |
| Group cardinality | DefraDB `_count` (`query-parse/src/query_parse/aggregates.rs`; valid with no field argument) |
| "Has this already fired?" | `AgentRequest` query on `caused_by_trigger_id` — the gate already runs this exact query |
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

Eight hops, each an existing seam:

```text
source doc[correlation_field]
  │  (1) already in doc_vars — zero extra queries
  ▼
FireIntent.correlation
  │  (2) mod.rs:46
  ▼
TriggerLockKey + gate filter
  │  (3) mod.rs:23   (4) production_materializer.rs:171,234
  ▼
AgentRequest.caused_by_correlation
  │  (5) lifecycle::TriggerLineage
  ▼
ToolRuntimeScope.correlation
  │  (6) tool_call_lifecycle/runtime.rs:163 — one more field on the task-local
  ▼
BoundedWriteTool stamps declared fill fields
  │  (7) defra_write/mod.rs:130
  ▼
next trigger reads it off the source doc      (8) loop closed
```

| # | File | Change |
| --- | --- | --- |
| 1 | `trigger_engine/event_source.rs:618` | read `doc_vars[correlation_field]` |
| 2 | `trigger_engine/mod.rs:46` | `FireIntent.correlation: Option<String>` |
| 3 | `trigger_engine/mod.rs:23` | `TriggerLockKey` → `(String, String, TriggerKind, Option<String>)` |
| 4 | `trigger_engine/production_materializer.rs:171,234` | gate methods take `correlation: Option<&str>`; filters gain `caused_by_correlation: { _eq: … }` **only when `Some`** |
| 5 | `lifecycle::TriggerLineage` | carries `correlation`; written to the request |
| 6 | `tool_call_lifecycle/runtime.rs:163` | `ToolRuntimeScope.correlation: Option<String>` |
| 7 | `defra_write/mod.rs:130` | `build_mutation` stamps fill fields from `current_tool_runtime_context()` |
| 8 | — | closes at hop 1 for the next trigger |

**Backward compatibility is structural.** An `EventTrigger` with no
`correlation_field` yields `None` at every hop; the gate filter omits its
clause; behavior is byte-identical to today. This is not a compatibility
shim — it is the `Option` being `None`.

## Surface

### `EventTrigger` (existing collection, new columns)

| Field | Meaning |
| --- | --- |
| `correlation_field` | Field on the source doc identifying the run. Scopes the gate and groups fires. |
| `fire_mode` | `per_document` (default, today's behavior) or `per_group` |
| `expected_count` | Literal group width — fixed-width fan-out (3 verifiers, 5 judges) |
| `expected_count_field` | Field on the source doc carrying the width — producer-decided |
| `expected_count_from` | Collection whose same-tag docs are counted — batch map-reduce (see soundness condition below) |
| `group_timeout_secs` | Fire with the partial group this long after the group's first-seen |
| `group_min_count` | Floor for a timeout fire; below it, expire without firing |

Exactly one of the three `expected_count*` sources may be set, and only under
`per_group`.

### `AgentRequest` (existing collection, one new column)

```graphql
caused_by_correlation: String @index @immutable
```

Follows the `caused_by_source_doc_id` precedent exactly: lineage, indexed,
immutable.

### `WriteToolField` (no schema change)

`write_tools` and `DatastoreToolSurface.entries` are `[String]` columns of JSON,
so a new optional key on the decl needs no schema migration.

```json
{ "name": "run_id",         "fill": "correlation" }
{ "name": "expected_total", "fill": { "source_field": "expected_total" } }
```

A filled field is:

- **omitted from the model-visible JSON schema** in `BoundedWriteTool::definition()`
- **rejected** if the model passes it anyway
- filled by the runtime at `call()` time

So the tag cannot be forged, and cannot be forgotten. This is the property that
makes graphs composable: correlation is a runtime invariant, not prompt
discipline.

`Fill::SourceField` copies a named field from the *triggering source doc*
through the agent hop. It exists because the alternative — instructing a model
to retype `expected_total` on every write — reintroduces exactly the fragility
the tag removes, for exactly the value a barrier depends on.

### Template scope

Beside today's `{{ doc.* }}` / `{{ event.* }}` / `{{ node.* }}` / `{{ ctx.* }}`:

- `{{ event.correlation }}` — available in both fire modes
- `{{ group.correlation_value }}`, `{{ group.count }}`, `{{ group.docs }}`
  (projected source docs), `{{ group.complete }}` (`false` on a timeout fire)
  — `per_group` only

## Fire semantics

### Completion is evaluated in the event source, not in `dispatch`

An incomplete group emits **no `FireIntent` at all**. Routing it through
`dispatch` to be `Skipped` would make every non-final member write
`last_status: "skipped"` and bump the trigger's runtime fields, so `fire_count`
and `last_error` would stop meaning anything operationally.

### Idempotence needs no persisted marker

Before a group fires, query whether an `AgentRequest` exists with
`(caused_by_trigger_id, caused_by_correlation)` in **any** lifecycle state.
Non-empty → do not fire.

This answers the issue's open durability question without storing group state:
a node restart can neither double-fire a completed group nor lose a pending
one, because the marker is the durable request row itself. Membership rebuilds
from the ordinary seed scan.

Single-writer safety: the gate is `agent_did`-scoped (#605) and each
`(did, behavior)` runs on exactly one deployment, so no second node races the
same group. Within the process, the tag-keyed `TriggerLockKey` covers
concurrent members.

### Counting

`_count` over the source collection filtered by the tag. If root-level
`_count` with a filter does not hold up in practice, the fallback is the
`limit`-capped `_docID` scan the event source already performs in
`load_doc_ids_for_collection` — same cost class as the existing rescan.

### Soundness condition on `expected_count_from`

`expected_count_from` compares `count(source docs with tag)` against
`count(upstream docs with tag)`. This is **only sound when the upstream set is
complete before any downstream member can appear.**

Counter-example, and the reason this is called out rather than assumed: if a
recon stage writes `ReviewArea` docs one at a time, a scan of area 1 can finish
before area 5 is created. Triage then observes `count(ScanResult) == 1 ==
count(ReviewArea)` and fires on a partial group.

Use `expected_count_from` only for batch producers whose whole output set lands
before downstream work starts. For incremental producers use
`expected_count_field` with `Fill::SourceField` propagation. `config validate`
cannot detect the difference; the field documentation must carry the warning.

### Timeout

Rides the existing 5s `rescan_tick`. Group first-seen is in-memory, so a
restart **restarts the timeout clock**: a partial fire is delayed, never
duplicated (the idempotence query holds). That is the accepted cost of keeping
zero persisted group state.

`group_min_count` gates the timeout fire: below the floor the group expires
silently. Expiry is also in-memory, so a restart re-arms and re-expires — no
fire either way, so the outcome is unchanged.

### Concurrency gate, with the tag

- `serial` — one in-flight fire per `(trigger, run)`. Concurrent runs no longer
  drop each other's fires.
- `latest_only` — supersedes only within the run. Concurrent runs no longer
  cancel each other's work.
- `parallel` — unchanged.

The existing expired-claim carve-out (`row_gates_serial_fire`,
`production_materializer.rs:87`) is unaffected and composes: a past-deadline
orphan still does not gate, now scoped to its own run.

## Validation

`desired_state/validate.rs` (~line 698), plus the runtime backstop at snapshot
build:

- `fire_mode: per_group` without `correlation_field` → reject
- more than one of `expected_count` / `expected_count_field` /
  `expected_count_from` → reject
- any `expected_count*` under `per_document` → reject
- `group_timeout_secs` / `group_min_count` under `per_document` → reject
- `group_min_count` without `group_timeout_secs` → reject
- `correlation_field` must pass a field-identifier check — it is interpolated
  into filter fragments, so it gets the same defense class as
  `validate_collection_identifier` (see the GraphQL sharp edge in `CLAUDE.md`)
- a `WriteToolField` with any `fill` must not be `required` — the model cannot
  supply it
- live validate: `expected_count_from` names a collection that exists on the
  node, same class as the existing source-collection probe

### Fail-closed, both directions

- **Tag missing on the source doc** while `correlation_field` is declared →
  skip the fire; `last_status` / `last_error` name the field. Firing untagged
  would silently rejoin unrelated runs inside the gate, which is the bug being
  fixed.
- **`fill: correlation` with no ambient tag** (e.g. someone manual-runs the
  behavior) → the write fails with an error naming the trigger, rather than
  quietly creating a document orphaned from every group.

### Self-config

`fill` is a `write_tools` / surface-entry concern, and `write_tools` is already
in the protected, never-self-patchable set (`self_config/mod.rs`). No new
self-config category. An agent must not be able to grant itself a tool that
stamps a tag of its choosing.

## Proofs

Per the foundation flow, the theorem restatement **is** the specification of
the fix and lands first.

- `Proofs/Triggers/Serial.lean`, `Proofs/Triggers/LatestOnly.lean` — today's
  mutual-exclusion theorems are stated over `(did, trigger, kind)`; they gain a
  correlation dimension. Restatement, not new obligation.
- `Proofs/Triggers/Lineage.lean` — the tag joins the stamped lineage.
- **New obligation: exactly-once per group.** The whole idempotence argument
  rests on "the request-existence query is a sound fired-marker," and that is
  worth discharging before the Rust exists.
- `Proofs/EventDelivery` — `EventSource`'s `EventDeliverySourceContract`
  (`dedupe_policy: "monotone_once"`, `event_source.rs:737`) becomes
  monotone-once **per group**; the contract constant must say so.

## Testing

Conformance tests driven from the spec change, plus e2e beside
`crates/gents/tests/e2e_triggers/event_trigger_e2e.rs`. The load-bearing cases:

- Two runs interleaved through one `serial` trigger: no skip and no
  supersession attributable to a different correlation value.
- `correlation_field` + a count source: fires exactly once per group, after the
  last member create, with the full group in scope.
- A group that never completes: one fire at `group_timeout_secs` with
  `group.complete == false`, or expiry without firing below `group_min_count`.
- Restart mid-group: neither double-fires a group that already fired nor loses
  one that had not.
- Triggers without the new fields: byte-identical behavior to today.
- `config validate` rejects each invalid combination above.

Gate with the full `cargo test -p gents` suite and
`cargo check --workspace --all-targets`, per `CLAUDE.md`.

## Acceptance: `demo/code-review`

A self-contained pack with real (non-toy) stages that reviews **this
repository** by default, following the `demo/pipeline` layout precedent.

```text
ReviewJob (seed)  ──►  recon   ──► N × ReviewArea      fan-out
ReviewArea        ──►  scan    ──► Finding*, ScanResult    per_document, tagged
ScanResult        ──►  triage  ──► TriageReport        per_group
```

- **recon** partitions the repo into review areas and stamps `expected_total`
  on each `ReviewArea`.
- **scan** fires once per area, reads code in that area, writes zero or more
  `Finding` docs and exactly one `ScanResult` sentinel. Its write tools carry
  `fill: correlation` and `fill: {source_field: "expected_total"}`, so neither
  value is the model's responsibility.
- **triage** is `per_group` on `ScanResult` with `expected_count_field:
  "expected_total"`, `concurrency: serial`, `correlation_field: "run_id"`. It
  fires once with the whole group and reads the run's findings via
  `defra_query`.

The `ScanResult` sentinel stays — one create by an agent that already has a
write tool is a legitimate "worker finished" signal. What the feature removes
is the **gate behavior** (one inference call per sentinel to compute a
`COUNT(*)`, a model deciding a numeric comparison) and the **cron backstop**
(replaced by `group_timeout_secs`). That is the issue's actual complaint, and
the claim should not be stated more broadly than that.

Scan stages need `enable_file_tools` and `enable_bash` to read the repo — they
are not datastore-only agents. `triage` gets `defra_query` scoped to the pack's
collections and no file tools.

Because the pack runs against this repo, it is tunable in place: real findings
on real code are the signal that the graph works, not a synthetic fixture.

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
- **Persisted group state.** See the timeout tradeoff above — accepted
  knowingly.
- **`{{ args.* }}` in event-trigger scope.** Unchanged from the predecessor
  design.

## Slicing

| PR | Contents |
| --- | --- |
| **A** | Lean restatement (`Serial`, `LatestOnly`, `Lineage`) + tag spine hops 1–5 + `caused_by_correlation`. Fixes the gate bug alone, with no new fire semantics — independently landable and worth landing first. |
| **B** | Hops 6–8: `ToolRuntimeScope.correlation`, `fill` on `WriteToolField`, `build_mutation` stamping. Closes the loop so tags survive agent hops. |
| **C** | `per_group`: the three count sources, timeout / min-count, `{{ group.* }}`, the exactly-once proof, the `EventDelivery` contract update, `config validate` rules. |
| **D** | `demo/code-review` pack + operator README. |

No intermediate that ships a stored-but-unused field: a column that does
nothing invites drift and silent no-op configs.

## Success criteria

- Two runs interleaved through the same graph never interfere.
- A `per_group` trigger fires exactly once per group with the full group in
  scope, and no agent computes a count.
- A group that never completes fires once at timeout with
  `group.complete == false`, or expires below `group_min_count`.
- A restart mid-group neither double-fires nor loses a group.
- Triggers without the new fields behave exactly as today.
- `config validate` rejects every invalid field combination listed above.
- `demo/code-review` finds real issues in this repository, with a barrier stage
  that is a plain agent containing no counting logic.
