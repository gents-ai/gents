# Event-trigger graph experiments — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an `experiments/` tree of EventTrigger-driven graph shapes
(desired-state arms, config + docs only) plus one thin e2e wrapper in
`crates/gents/tests/e2e_triggers/` that spins up a node, sends a **single
GraphQL seed write**, and asserts trigger lineage — without
`fan_out_and_synthesize` and without any new harness code.

**Architecture:** Document pipeline. Nodes = Tasks/behaviors. Edges =
EventTriggers on `created`. Kick = one `create_ExperimentJob`. Measure =
trigger lineage + existing `gents trace timeline|project`. All node
spin-up, mock-model, and await machinery is reused from
`crates/gents/tests/support/`.

**Tech Stack:** Rust integration tests (`crates/gents/tests`,
`crates/gents-cli/tests`), desired-state JSON manifest roots
(`gents config apply|validate`), GraphQL SDL + mutations, Markdown docs.

**Spec:** `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`

## Global Constraints

- Docs, config, and test code only — no runtime or Lean changes in v1.
- EventTrigger v1: `event_kind = "created"` only; never chain on
  `AgentResponse` / lifecycle updates; no fan-in barrier.
- Event-trigger prompt templates may use `{{ doc.* }}` and `{{ event.* }}`
  (plus `node.node_did` / `node.behavior_id` / `ctx.now`); `{{ args.* }}`
  is currently rejected by the validator in trigger scope. v1 arms don't
  need it (the seed doc carries every run parameter); exposing `args` to
  trigger scope is an accepted runtime follow-up if a later arm does (see
  design Decision 6).
- The experiments double as a test of the template system: every arm's
  prompt templates should exercise `doc` + `event` + `node` + `ctx` vars,
  and the e2e asserts on the rendered content.
- `orchestration_enabled: false` on every experiment tool selection.
- Gate with `cargo test -p gents --test e2e_triggers` (never `--lib`) and
  `cargo test -p gents-cli --test cli_experiment_shapes`.
- Do not commit `experiments/runs/` artifacts or secrets.
- Each task leaves the tree buildable; commit at the end of every task.

---

### Task 1: `experiments/` layout — README, schemas, runs scratch dir

**Files:**
- Create: `experiments/README.md`
- Create: `experiments/schemas/experiment_job.graphql`
- Create: `experiments/schemas/experiment_finding.graphql`
- Create: `experiments/runs/.gitignore`

**Interfaces:**
- Produces: collection names `ExperimentJob` (fields `job_id`, `prompt`,
  `suite`, `arm`) and `ExperimentFinding` (fields `job_id`, `finding_id`,
  `content`, `stage`) — Tasks 2 and 3 depend on these exact names.

- [ ] **Step 1: Write the SDL files**

`experiments/schemas/experiment_job.graphql`:

```graphql
type ExperimentJob {
  job_id: String
  prompt: String
  suite: String
  arm: String
}
```

`experiments/schemas/experiment_finding.graphql`:

```graphql
type ExperimentFinding {
  job_id: String
  finding_id: String
  content: String
  stage: String
}
```

Keep them registrable via `node.add_schema(sdl)` in tests — same style as
the inline `WebhookEvent` SDL in `event_trigger_e2e.rs`.

- [ ] **Step 2: Write `experiments/runs/.gitignore`**

```gitignore
*
!.gitignore
```

- [ ] **Step 3: Write `experiments/README.md`** covering, in order:
  - the document-pipeline model: nodes = Tasks, edges = EventTriggers on
    `created` only; no barrier; no response-lifecycle edges
  - the operator flow (four steps): `gents config apply --root
    experiments/shapes/<arm> --home <home> --bind-agent-did home`; register
    `experiments/schemas/*.graphql` on the node; POST the kick mutation
    from the design spec with a fresh `job_id`; poll
    `AgentRequest(filter: {caused_by_trigger_id: {_eq: "…"}})` and export
    traces into `experiments/runs/<job_id>/`
  - pointer to the design spec and to the e2e wrapper as the CI fence

- [ ] **Step 4: Commit**

```bash
git add experiments/
git commit -m "$(cat <<'EOF'
Add experiments/ layout: seed schemas, operator README, runs scratch dir.

Document the EventTrigger document-pipeline model and the single GraphQL
seed write as the only kickoff.
EOF
)"
```

---

### Task 2: Three arm roots + shape-validity fence

**Files:**
- Create: `experiments/shapes/single-loop/` (desired-state root)
- Create: `experiments/shapes/fanout-on-job/`
- Create: `experiments/shapes/pipeline-two-stage/`
- Create: `crates/gents-cli/tests/cli_experiment_shapes.rs`

**Interfaces:**
- Consumes: collection names from Task 1.
- Produces: arm names `single-loop`, `fanout-on-job`, `pipeline-two-stage`;
  trigger ids `exp-single`, `exp-fan-a` / `exp-fan-b` / `exp-fan-c`,
  `exp-stage1` / `exp-stage2` (Task 3 mirrors these shapes inline).

Each root uses the exact on-disk layout `desired_state/write.rs` produces
(note `event_triggers/` uses an underscore). Start every root by copying
the complete worked fixture in
`crates/gents-cli/tests/cli_config_validate.rs` (the tasks + schedules
root, around line 322) and editing it down — that fixture, not this plan,
is the source of truth for required ToolSelection / AgentBehavior /
InferenceBackend fields. `gents config validate` is static and needs no
server.

- [ ] **Step 1: Author `single-loop`** — one behavior, one Task, one
      EventTrigger:

```text
single-loop/
  agent-principal.json
  inference-backends/exp-backend/object.json
  tool-selections/exp-tools/object.json
  agent-behaviors/exp-node/object.json  (+ system_prompt.md)
  tasks/exp-single-task/object.json     (+ prompt.md)
  event_triggers/exp-single/object.json
```

`tasks/exp-single-task/object.json` (prompt spilled to sidecar):

```json
{
  "task_id": "exp-single-task",
  "name": "exp-single-task",
  "behavior_id": "exp-node",
  "prompt_template": "./prompt.md",
  "enabled": true
}
```

`tasks/exp-single-task/prompt.md` — exercises every trigger-scope
template root on purpose:

```text
Job {{ doc.job_id }} (arm {{ doc.arm }}, suite {{ doc.suite }}) fired by
trigger {{ event.trigger_id }} on {{ event.source_collection }} doc
{{ event.source_doc_id }} at {{ ctx.now }}, handled by behavior
{{ node.behavior_id }} on {{ node.node_did }}.

{{ doc.prompt }}
```

`event_triggers/exp-single/object.json`:

```json
{
  "trigger_id": "exp-single",
  "task_id": "exp-single-task",
  "source_collection": "ExperimentJob",
  "event_kind": "created",
  "enabled": true,
  "concurrency": "parallel"
}
```

Tool selection: `orchestration_enabled: false`, no bash, no file tools.

- [ ] **Step 2: Author `fanout-on-job`** — same backend/tool-selection
      docs; three behaviors (`exp-fan-a/b/c`) with distinct
      `system_prompt.md` framings, three Tasks, three EventTriggers
      (`exp-fan-a/b/c`) all on `source_collection: "ExperimentJob"` with no
      filter. Every prompt template includes `{{ doc.job_id }}`.

- [ ] **Step 3: Author `pipeline-two-stage`** — stage-1 trigger
      `exp-stage1` on `ExperimentJob` whose tool selection declares a
      bounded write tool for `ExperimentFinding` (copy the `write_tools`
      declaration shape from `write_tool_trigger_e2e.rs` / the validate
      fixture); stage-2 trigger `exp-stage2` on
      `source_collection: "ExperimentFinding"` whose prompt template chains
      `{{ doc.job_id }}` and `{{ doc.content }}`.

- [ ] **Step 4: Write the fence test** —
      `crates/gents-cli/tests/cli_experiment_shapes.rs`: iterate
      `../../experiments/shapes/*/` from `CARGO_MANIFEST_DIR`, call
      `load_manifest_root` on each, assert an empty error list. This keeps
      checked-in arms from rotting silently.

- [ ] **Step 5: Run the fence and the CLI validate command**

```bash
cargo test -p gents-cli --test cli_experiment_shapes
gents config validate --root experiments/shapes/single-loop
gents config validate --root experiments/shapes/fanout-on-job
gents config validate --root experiments/shapes/pipeline-two-stage
```

Expected: all pass / report `valid`.

- [ ] **Step 6: Commit**

```bash
git add experiments/shapes crates/gents-cli/tests/cli_experiment_shapes.rs
git commit -m "$(cat <<'EOF'
Add three experiment arms as desired-state roots, with a validity fence.

single-loop, fanout-on-job, and pipeline-two-stage express topology as
Tasks + EventTriggers; a gents-cli test validates every checked-in root.
EOF
)"
```

---

### Task 3: The thin wrapper — e2e fan-out + pipeline on one seed write

**Files:**
- Create: `crates/gents/tests/e2e_triggers/experiment_graph_e2e.rs`
- Modify: `crates/gents/tests/e2e_triggers.rs` (add
  `#[path = "e2e_triggers/experiment_graph_e2e.rs"] mod experiment_graph_e2e;`)

**Interfaces:**
- Consumes: SDL field names from Task 1; the shapes from Task 2 are
  mirrored inline (seeded via `create_Task` / `create_EventTrigger`
  mutations, the way `event_trigger_e2e.rs` does) — the e2e does not read
  `experiments/shapes/` from disk; Task 2's fence covers applyability.

Copy the proven recipe from `event_trigger_e2e.rs` verbatim: `test_db` →
`node.add_schema` for both experiment SDLs → `MockModelEndpoint::start` +
`bind_default_behavior_backend` →
`Gents::from_default_behavior_documents` spawned with a shutdown watch
channel → wait for `process_state == "ready"` via
`fetch_runtime_snapshot`, capture `initial_generation` → seed Task +
EventTrigger docs → wait `active_generation > initial_generation` → act →
assert.

- [ ] **Step 1: Write the fan-out test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_seed_create_fans_out_to_three_triggers() {
    // boot as above; seed three Tasks + three EventTriggers, all on
    // ExperimentJob/created, prompt templates containing {{ doc.job_id }}
    // ONE mutation: create_ExperimentJob(job_id, prompt, suite, arm)
    // for each trigger id: poll AgentRequest(filter:
    //   {caused_by_trigger_id: {_eq: id}}) until one row or 30s deadline
    // assert per request: caused_by_trigger_kind == "event",
    //   content contains the job_id AND that request's own trigger id
    //   (rendered from {{ event.trigger_id }} — per-trigger template
    //   render, the multi_field_template pattern from
    //   write_tool_trigger_e2e.rs)
    // assert per trigger: fire_count == 1, last_status == "fired"
}
```

- [ ] **Step 2: Run it**

```bash
cargo test -p gents --test e2e_triggers experiment_graph -- --nocapture
```

Expected: FAIL until wired, then PASS.

- [ ] **Step 3: Write the pipeline test** — combine with the
      `write_tool_trigger_e2e.rs` pattern:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipeline_stage_two_fires_on_finding_create() {
    // stage-1 trigger on ExperimentJob; behavior's tool selection declares
    // a BoundedWriteTool for ExperimentFinding; mock model responds with
    // that tool call (write_tool_trigger_e2e.rs shows the mock shape)
    // stage-2 trigger on ExperimentFinding
    // ONE create_ExperimentJob → await stage-1 request → await the
    // finding doc → await stage-2 request
    // assert stage-2 content contains the finding's job_id + content,
    // and stage-2 caused_by_trigger_id is the stage-2 trigger — proving
    // the edge was the document create, not a lifecycle update
}
```

- [ ] **Step 4: Run the full trigger suite**

```bash
cargo test -p gents --test e2e_triggers -- --nocapture
```

Expected: PASS, including the pre-existing trigger tests.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/tests/e2e_triggers.rs \
  crates/gents/tests/e2e_triggers/experiment_graph_e2e.rs
git commit -m "$(cat <<'EOF'
Add experiment-graph e2e: one seed write fans out and chains stages.

Locks the single-kick → lineage contract: N triggers on one
ExperimentJob create, and stage-2 firing only on ExperimentFinding
creates.
EOF
)"
```

---

### Task 4: Measurement docs + final verification

**Files:**
- Modify: `experiments/README.md` (measurement section)

- [ ] **Step 1: Add the measurement section** — exact commands:

```bash
gents trace timeline --request-id <id> --home <home>
gents trace project --projection multi-agent --format eval-jsonl --request-id <id> --home <home>
```

State what we trust for cost/structure: request count by
`caused_by_trigger_id`, inference call count + wall time from the
timeline, and token usage from `InferenceCall.prompt_tokens` /
`completion_tokens` / `cached_input_tokens`. State explicitly that
`AgentResponse.token_count` is a streaming word-count proxy (0 on
recovered responses) and must not be used as a cost metric. Quality
scoring is out of band via eval-jsonl.

- [ ] **Step 2: Run the gates**

```bash
cargo test -p gents --test e2e_triggers
cargo test -p gents-cli --test cli_experiment_shapes
cargo check --workspace --all-targets
```

Expected: all green (the workspace check is cheap insurance even though
no production code changed).

- [ ] **Step 3: Confirm** no secrets, no `experiments/runs/*` content
      beyond `.gitignore`, plan checkboxes updated.

- [ ] **Step 4: Commit**

```bash
git add experiments/README.md docs/superpowers/
git commit -m "$(cat <<'EOF'
Document experiment measurement via timeline and InferenceCall tokens.
EOF
)"
```

---

## Implementation PR sequencing

| PR | Contents |
| --- | --- |
| **This PR (docs)** | Design + plan only |
| PR A | Tasks 1–2: `experiments/` tree + arms + validity fence |
| PR B | Tasks 3–4: e2e wrapper + measurement docs |

## Explicitly deferred

- `event_kind: updated` engine support; barrier / join triggers
- Live-LLM quality A/B suite (judge scoring stays offline)
- Promoting `ExperimentJob` into `gents-schemas` product schemas
- Cross-node P2P arms (extend the wrapper with `test_p2p_db` + the
  replicator helper from `event_trigger_p2p_e2e.rs`)
- Any runner/harness code under `experiments/` (operator flow stays four
  documented CLI/GraphQL steps)

## References

- Design: `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`
- E2E templates: `crates/gents/tests/e2e_triggers/event_trigger_e2e.rs`,
  `write_tool_trigger_e2e.rs`; helpers in `crates/gents/tests/support/`
- Desired-state layout + validation: `crates/gents-cli/src/desired_state/{write,validate}.rs`
- Worked manifest fixture: `crates/gents-cli/tests/cli_config_validate.rs`
- Config CLI: `gents config apply|diff|export|validate`
- Trace CLI: `gents trace timeline|project`
