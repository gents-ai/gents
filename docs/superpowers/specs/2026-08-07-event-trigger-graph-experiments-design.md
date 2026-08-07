# Event-trigger graph experiments (design)

## Problem

We want a **repeatable, version-controlled way to compare multi-agent
topologies** on Gents — not as ephemeral in-loop orchestration, but as
document-driven graphs:

- **Nodes** = Tasks bound to behaviors (prompt + model + tools)
- **Edges** = EventTriggers on collection creates
- **Shared state** = source documents (`{{ doc.* }}` in prompt templates)
- **Kickoff** = a single GraphQL create of a seed document
- **Measurement** = `run_timeline` + adapter projections

The deliverable is deliberately small, because substantially all the
infrastructure already exists in the repo:

1. **A set of agent config files per experiment** ("arms") — desired-state
   roots under `experiments/shapes/`, applied with `gents config apply`.
2. **A thin wrapper over the existing e2e trigger harness** — one test file
   that spins up a node, seeds an arm's documents, sends a single kickoff
   write, and awaits trigger lineage.

Node spin-up, the mock model endpoint, runtime-snapshot waits, lineage
queries, and timeline/projection export are all existing runtime, CLI, or
test-support code. This workstream adds **config and docs plus one e2e
file** — no new harness code.

This design deliberately **does not** use `fan_out_and_synthesize` for
experiment topology. Fan-out becomes "N EventTriggers on the same seed
create." Pipeline stages become "stage agents create next-collection docs."
Barrier / fan-in is **out of scope for v1** (see Non-goals).

## Constraints from the runtime (v1 EventTrigger)

These are product facts, not preferences:

| Constraint | Implication for graphs |
| --- | --- |
| `event_kind` is **`created` only** (first-seen) | Edges fire on **new documents**, never on in-place status updates |
| `AgentResponse` / `AgentRequest` are created then updated | Do **not** chain stages on "response completed" or lifecycle transitions |
| Filter is a GraphQL fragment on the source doc | Stage routing = fields on the seed / artifact docs |
| Concurrency is **per trigger** (`parallel` / `serial` / `latest_only`) | No multi-child barrier; join is not a trigger feature |
| Multiple triggers may match one create | Native **fan-out** |
| Materialized requests stamp `caused_by_trigger_id` + `caused_by_trigger_kind: "event"` | Measurement and await use trigger lineage |
| Event-trigger templates see `{{ doc.* }}` and `{{ event.* }}` (plus `node.node_did`, `node.behavior_id`, `ctx.now`); `{{ args.* }}` is **manual-run only** | Seed fields must carry `job_id`, prompt, arm labels |
| First-seen tracking seeds forward-only with a scan cap (`event_source.rs`) | Use fresh, experiment-only collections as trigger sources |

**Conclusion:** experiment graphs are **document pipelines**. Each stage that
should fire a later stage must **create** a document in a watched
collection.

## Architecture

```text
experiments/                      config + docs only — no code
  README.md                       operator guide: apply → kick → await → export
  schemas/                        SDL for ExperimentJob / ExperimentFinding
  shapes/<arm>/                   one desired-state root per arm
  runs/                           gitignored scratch for trace exports

crates/gents/tests/e2e_triggers/
  experiment_graph_e2e.rs         the thin wrapper (mock model; CI fence)
```

```text
                    ┌─ Task/behavior A  (EventTrigger 1)
 seed create ───────┼─ Task/behavior B  (EventTrigger 2)   ← fan-out
                    └─ Task/behavior C  (EventTrigger 3)

 stage agent creates ExperimentFinding docs
        │
        └─► next EventTriggers (pipeline)
```

The wrapper is a normal integration test in the existing `e2e_triggers`
target so it inherits `tests/support/` (embedded node via `test_db`,
`MockModelEndpoint`, runtime-snapshot waits, lineage queries) for free.
That support tree is not reachable from examples or a new crate — another
home would mean re-implementing the mock endpoint. Multi-node arms, when
they come, extend the same wrapper with the existing `test_p2p_db` +
replicator helpers.

### Seed document = experiment handle

One shared seed collection, `ExperimentJob`, holds:

| Field | Purpose |
| --- | --- |
| `job_id` | Stable run id; greppable in prompts and lineage queries |
| `prompt` | Task body for templates (`{{ doc.prompt }}`) |
| `suite` | Experiment suite name (e.g. `topology-ab`) |
| `arm` | Which shape was applied |

Kickoff is intentionally one mutation:

```graphql
mutation {
  create_ExperimentJob(input: {
    job_id: "exp-…"
    prompt: "…"
    suite: "topology-ab"
    arm: "fanout-on-job"
  }) { _docID }
}
```

### Arms (initial set)

| Arm | Topology | Expected fires |
| --- | --- | --- |
| `single-loop` | One EventTrigger on `ExperimentJob` created → one task | 1 request |
| `fanout-on-job` | N EventTriggers on the same `ExperimentJob` create | N requests, same `job_id` |
| `pipeline-two-stage` | Stage-1 on seed; stage-1 writes `ExperimentFinding` docs; stage-2 EventTrigger on finding created | ≥1 stage-1 + stage-2 per finding |

`single-loop` kicks through a trigger (not a direct `AgentRequest` create)
so every arm shares the same kick API. Arms share backends and tool
selections where possible; the topology delta should read as a clean
`gents config diff` between shape roots.

Arm prompt templates deliberately exercise the full trigger template
surface — multiple `{{ doc.* }}` fields, `{{ event.* }}`
(`trigger_id`, `source_collection`, `source_doc_id`, `fired_at`),
`node.node_did` / `node.behavior_id`, `ctx.now` — so the experiments
double as a live test of the template system, and the e2e asserts on the
rendered result.

### Config surface

Each arm is a desired-state root in the layout `gents config export`
writes and `apply` / `validate` / `diff` read:

```text
shapes/<arm>/
  agent-principal.json
  inference-backends/<backend_id>/object.json
  tool-selections/<selection_id>/object.json     orchestration_enabled: false
  agent-behaviors/<behavior_id>/object.json      (+ system_prompt.md sidecar)
  tasks/<task_id>/object.json                    (+ prompt.md sidecar)
  event_triggers/<trigger_id>/object.json        note: underscore dir name
```

```bash
gents config validate --root experiments/shapes/<arm>                      # static, no server needed
gents config apply    --root experiments/shapes/<arm> --home <home> --bind-agent-did home
```

`--bind-agent-did home` rebinds the root's placeholder DID to the target
home, so checked-in arms apply to any node. A complete worked root in this
exact layout lives at `crates/gents-cli/tests/cli_config_validate.rs`
(tasks + schedules variant); `cli_config_apply_e2e.rs` already covers
applying trigger roots.

### Running an experiment

**CI / mock (the wrapper):**

```bash
cargo test -p gents --test e2e_triggers experiment_graph
```

The wrapper follows the proven `event_trigger_e2e.rs` recipe: `test_db` →
register experiment SDL → mock endpoint + behavior binding → `Gents` run →
seed the arm's Task/EventTrigger docs → wait for the reconcile generation
bump → **one** seed create → poll `AgentRequest` by `caused_by_trigger_id`
→ assert lineage, rendered content, and trigger `fire_count`.

**Operator (real home, real or mock backend)** — four documented steps, no
new code:

1. `gents config apply --root experiments/shapes/<arm> --home <home> --bind-agent-did home`
2. Ensure `experiments/schemas/*.graphql` are registered on the node
3. POST the kick mutation (curl or GraphQL client) with a fresh `job_id`
4. Poll `AgentRequest(filter: {caused_by_trigger_id: …})`, then export
   traces (below) into `experiments/runs/<job_id>/`

### Measurement

After a run:

```bash
gents trace timeline --request-id <id> --home …
gents trace project --projection multi-agent --format eval-jsonl --request-id <id> --home …
```

Cost/structure metrics we trust (v1):

- request count / sibling count by `caused_by_trigger_id`
- inference call count and wall time from the timeline
- **token usage from `InferenceCall.prompt_tokens` / `completion_tokens` /
  `cached_input_tokens`** — not `AgentResponse.token_count`, which is a
  streaming word-count proxy and reads 0 on recovered responses

Quality scoring (LLM-as-judge, human rubrics) is **out of band**: export
eval-jsonl; score offline.

## Non-goals (v1)

- New harness/runner code under `experiments/` — it is config + docs only
- Replacing or extending `fan_out_and_synthesize` barrier semantics
- `event_kind: updated` / "on lifecycle completed" triggers
- Claiming topology quality wins without a separate judge suite
- Cross-node P2P experiment arms (extend the wrapper with `test_p2p_db`
  later)

## Decisions

1. **Seed collections are experiment-local SDL** in `experiments/schemas/`
   (same move as the e2e custom `WebhookEvent` / `ActionRequest`); promote
   into `gents-schemas` only if productized.
2. **Stage-1 findings in CI come from the mock model's write-tool path**
   (`write_tool_trigger_e2e.rs` pattern); operator demos may inject
   findings via GraphQL when the model is weak.
3. **`single-loop` kicks through one trigger** for a uniform kick API.
4. **Manifests live at repo-root `experiments/`** so apply paths are short
   and runs are not mixed with design docs.
5. **The wrapper lives in `crates/gents/tests/e2e_triggers/`**, not a new
   crate or example — that is the only home where `tests/support/` is
   reachable.
6. **v1 needs no `{{ args.* }}` in trigger scope** — the seed document
   carries every run parameter, so `{{ doc.* }}` covers it. If a later arm
   needs run-scoped args, exposing `args` to event-trigger templates is an
   accepted runtime follow-up (validator scope in
   `desired_state/validate.rs` + var assembly in
   `trigger_engine/event_source.rs`), not a v1 blocker.

## Success criteria

- Three arms check in as desired-state roots and pass
  `gents config validate --root`
- One GraphQL seed create kicks the fan-out arm and produces N
  lineage-stamped requests (e2e, mock model)
- Pipeline arm demonstrates stage-2 firing only after a new
  `ExperimentFinding` doc is created
- Docs state clearly: created-only, no barrier, document-pipeline model

## Related code

- Trigger engine: `crates/gents/src/trigger_engine/` (first-seen semantics
  in `event_source.rs`)
- E2E templates: `crates/gents/tests/e2e_triggers/event_trigger_e2e.rs`,
  `write_tool_trigger_e2e.rs`; shared helpers in `crates/gents/tests/support/`
- Desired state: `crates/gents-cli/src/desired_state/` (layout in
  `write.rs`, checks in `validate.rs`); worked fixture in
  `crates/gents-cli/tests/cli_config_validate.rs`
- Timeline / projections: `crates/gents/src/run_timeline.rs`,
  `adapter_projection.rs`
- CLI: `gents config {export,diff,apply,validate}`, `gents trace {timeline,project}`
