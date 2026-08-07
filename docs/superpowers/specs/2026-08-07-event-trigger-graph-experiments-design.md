# Event-trigger graph experiments (design)

## Problem

We want a **repeatable, version-controlled way to compare multi-agent
topologies** on Gents — not as ephemeral in-loop orchestration, but as
document-driven graphs:

- **Nodes** = Tasks bound to behaviors (prompt + model + tools)
- **Edges** = EventTriggers on collection creates
- **Shared state** = source documents (`{{ doc.* }}` in prompt templates)
- **Kickoff** = a single GraphQL create of a seed document
- **Measurement** = `run_timeline` + adapter projections (multi-agent /
  LangGraph / eval-jsonl)

The article-shaped open question this supports (same task, same model, loop
vs graph, cost + structure, optional quality) needs three things Gents
already almost has:

1. **Declared arms** via desired-state config (`gents config export` /
   `diff` / `apply`)
2. **Graph execution** via Tasks + EventTriggers (not
   `fan_out_and_synthesize`)
3. **Harness** that reuses the existing e2e trigger patterns: seed schema →
   apply arm → one write → await lineage → export metrics

This design deliberately **does not** use `fan_out_and_synthesize` for
experiment topology. Fan-out becomes “N EventTriggers on the same seed
create.” Pipeline stages become “stage agents write next-collection docs.”
Barrier / fan-in is **out of scope for v1** (see Non-goals).

## Constraints from the runtime (v1 EventTrigger)

These are product facts, not preferences:

| Constraint | Implication for graphs |
| --- | --- |
| `event_kind` is **`created` only** (first-seen) | Edges fire on **new documents**, never on in-place status updates |
| `AgentResponse` / `AgentRequest` are created then updated | Do **not** chain stages on “response completed” or lifecycle transitions |
| Filter is a GraphQL fragment on the source doc | Stage routing = fields on the seed / artifact docs |
| Concurrency is **per trigger** (`parallel` / `serial` / `latest_only`) | No multi-child barrier; join is not a trigger feature |
| Multiple triggers may match one create | Native **fan-out** |
| Materialized requests stamp `caused_by_trigger_id` / `kind` | Measurement and await use trigger lineage |
| Prompt templates get `doc` + `event` + `args` + node/ctx | Seed fields must carry `job_id`, prompt, arm labels |

**Conclusion:** experiment graphs are **document pipelines**. Each stage that
should fire a later stage must **create** a document in a watched
collection (via write tools, a helper mutation, or a later harness step).

## Architecture

```text
experiments/
  shapes/<arm>/                 desired-state manifest root
    … behaviors, tool_selections, tasks, event_triggers …
  schemas/                      SDL for seed + stage artifact collections
  harness/                      apply → kick → await → export
  runs/                         gitignored run artifacts

                    ┌─ Task/behavior A  (EventTrigger 1)
 seed create ───────┼─ Task/behavior B  (EventTrigger 2)   ← fan-out
                    └─ Task/behavior C  (EventTrigger 3)

 stage agent creates Finding/Claim docs
        │
        └─► next EventTriggers (pipeline)
```

### Seed document = experiment handle

One shared seed collection (proposed name: `ExperimentJob`, or
`ResearchJob` if we keep the research framing) holds:

| Field | Purpose |
| --- | --- |
| `job_id` | Stable run id; greppable in prompts and metrics |
| `prompt` / `question` | Task body for templates (`{{ doc.prompt }}`) |
| `suite` | Experiment suite name (e.g. `topology-ab`) |
| `arm` / `shape` | Which manifest was applied |
| optional labels | `expected_fires`, `fanout_width` for the awaiter |

Kickoff is intentionally one mutation:

```graphql
mutation {
  create_ExperimentJob(input: {
    job_id: "exp-…"
    prompt: "…"
    suite: "topology-ab"
    arm: "fanout-on-job"
  }) { _docID job_id }
}
```

### Arms (initial set)

| Arm | Topology | Kick | Expected fires |
| --- | --- | --- | --- |
| `single-loop` | No EventTriggers; one behavior | Direct `AgentRequest` create **or** one trigger → one task | 1 request |
| `fanout-on-job` | N EventTriggers on `ExperimentJob` created | Seed create | N requests, same `job_id` in content |
| `pipeline-two-stage` | Stage-1 on seed; stage-2 on `ExperimentFinding` created | Seed create; stage-1 writes findings (write tool or mock) | ≥1 stage-1 + stage-2 per finding |

Arms share backends/profiles where possible. Topology delta should be
visible as a clean `gents config diff` between arm roots (or two behaviors
on one principal).

### Config surface

Use the existing desired-state path (not the legacy single-JSON
`config import` for day-to-day):

```bash
gents config apply --root experiments/shapes/<arm> --home <home>
gents config diff  --root experiments/shapes/<arm> --home <home>
```

Manifest includes at least:

- `AgentBehavior` (per-node prompts/models)
- `ToolSelection` (typically **orchestration off**; stage writers need
  bounded write tools or `defra_query` as designed)
- `Task` (`prompt_template` with `{{ doc.job_id }}`, `{{ doc.prompt }}`)
- `EventTrigger` (`source_collection`, `event_kind: created`, `filter`,
  `concurrency`)

Optional: `ProjectionAcpBinding` if eval export redaction is part of the
arm.

### Harness

Reuse patterns already proven in
`crates/gents/tests/e2e_triggers/event_trigger_e2e.rs` and
`write_tool_trigger_e2e.rs`:

1. Ensure seed/stage schemas exist on the node
2. Apply arm (or seed Task/EventTrigger docs the way e2e does for unit
   fidelity)
3. Wait for runtime snapshot: ready + triggers active (generation bump)
4. **Single GraphQL write** of the seed doc
5. Poll `AgentRequest` by `caused_by_trigger_id` (and/or content /
   metadata containing `job_id`) until expected terminal set or deadline
6. Emit run artifact: request ids, fire_counts, timeline JSON, multi-agent
   projection / eval-jsonl

Two consumers of the same harness core:

| Consumer | Role |
| --- | --- |
| **CI e2e** (`crates/gents/tests/…`) | Mock model, assert lineage + fan-out count + pipeline fire; no live LLM |
| **Operator / research harness** (`experiments/harness/`) | Real or mock backend; dump metrics for offline A/B |

The CI e2e is the correctness fence. The operator harness is the measurement
loop. They should share seed schema names, lineage conventions, and await
predicates.

### Measurement

After a run:

```bash
gents trace timeline --request-id <id> --home …
gents trace project --projection multi-agent --request-id <id> --format eval-jsonl …
```

Cost/structure proxies from timeline (v1 metrics, not quality):

- request count / child or sibling count by trigger id  
- inference call count and wall time  
- response `token_count` when populated (document gaps; do not invent a
  full Claude-Code-style meter in this workstream unless metering is
  already complete)

Quality scoring (LLM-as-judge, human rubrics) is **out of band**: export
eval-jsonl; score offline.

## Non-goals (v1)

- Replacing or extending `fan_out_and_synthesize` barrier semantics
- `event_kind: updated` / “on lifecycle completed” triggers
- Full deep-research (search → fetch budget → 3-vote skeptics) as a
  product workflow
- Claiming topology quality wins without a separate judge suite
- Cross-deployment experiment fleets (can be a later arm)

## Open questions (resolve in implementation plan)

1. **Seed collection name and ownership** — experiment-only SDL in
   `experiments/schemas/` vs a first-class schema under
   `crates/gents-schemas`. Recommendation: experiment-local SDL first
   (matches e2e custom `WebhookEvent` / `ActionRequest`); promote only if
   productized.
2. **How stage-1 writes findings in CI** — mock model tool call with write
   tools (like `write_tool_trigger_e2e`) vs harness post-step creating
   findings. Recommendation: mock write path for true pipeline e2e; harness
   helper for operator demos without a smart model.
3. **single-loop kick** — same seed create with one trigger vs direct
   request. Recommendation: one trigger for uniform harness kick API.
4. **Where manifests live** — `experiments/` at repo root vs under
   `docs/`. Recommendation: repo-root `experiments/` so apply paths are
   short and runs are not mixed with design docs.

## Success criteria

- Three arms check in as desired-state roots and apply cleanly
- One GraphQL seed create kicks fan-out arm and produces N lineage-stamped
  requests (e2e, mock model)
- Pipeline arm demonstrates stage-2 fire only after a new finding doc is
  created
- Harness writes a run directory with request ids + timeline/projection
  exports
- Docs state clearly: created-only, no barrier, document-pipeline model

## Related code

- Trigger engine: `crates/gents/src/trigger_engine/`
- Event source first-seen / created-only:
  `crates/gents/src/trigger_engine/event_source.rs`
- Desired state: `crates/gents-cli/src/desired_state/`
- E2E: `crates/gents/tests/e2e_triggers/`
- Timeline / projections: `crates/gents/src/run_timeline.rs`,
  `adapter_projection.rs`
- CLI: `gents config {export,diff,apply}`, `gents trace {timeline,project}`
