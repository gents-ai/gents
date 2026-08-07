# Event-trigger graph experiments — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a repeatable `experiments/` tree of EventTrigger-driven graph
shapes (desired-state arms), a kickoff harness that starts a run with a
**single GraphQL seed write**, and CI e2e coverage that reuses
`e2e_triggers` patterns — without using `fan_out_and_synthesize`.

**Architecture:** Document pipeline. Nodes = Tasks/behaviors. Edges =
EventTriggers on `created`. Shared state = source docs. Kick = one
`create_ExperimentJob` (name final in Task 1). Measure = await trigger
lineage + `run_timeline` / multi-agent projection.

**Tech Stack:** Rust e2e (`crates/gents/tests`), desired-state manifests
(JSON/YAML tree via `gents config apply`), GraphQL mutations, shell or small
Rust/Python harness under `experiments/harness/`, Markdown docs.

**Spec:** `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`

## Global Constraints

- **Not Lean-first** for the experiments tree and harness (docs + test
  fixtures + config manifests). Only open a Lean task if a runtime change
  is required (v1 should not need one).
- EventTrigger v1: **`event_kind = "created"` only**; no chaining on
  `AgentResponse` / lifecycle updates.
- **No fan-in barrier** in this workstream. Do not reimplement
  `fan_out_and_synthesize` as triggers.
- Prefer **orchestration_enabled = false** on experiment tool selections.
- Gate Rust e2e with `cargo test -p gents --test e2e_triggers …` (or the
  new test binary if split); never claim green on `--lib` alone.
- Final gate before merge of implementation PRs: `cargo check --workspace
  --all-targets` when any Rust production code changes.
- `tracing`, never `println`, in runtime code. Harness scripts may print
  operator-facing progress.
- Each task leaves the tree buildable; commit at the end of every task.
- Do not commit `experiments/runs/` artifacts or secrets.

---

### Task 1: Seed + stage SDL and experiments layout

**Files:**
- Create: `experiments/README.md`
- Create: `experiments/schemas/experiment_job.graphql` (or `.sdl` fragment
  used by e2e and harness)
- Create: `experiments/schemas/experiment_finding.graphql` (pipeline stage-2
  source)
- Create: `experiments/shapes/.gitkeep` (or first arm stubs)
- Create: `experiments/runs/.gitignore` (`*` + `!.gitignore`)

**Decisions locked here:**
- Collection names: `ExperimentJob`, `ExperimentFinding` (unless a naming
  conflict appears — document any rename in README).
- `ExperimentJob` fields: `job_id`, `prompt`, `suite`, `arm` (all String;
  index `job_id` / `arm` if the store allows).
- `ExperimentFinding` fields: `job_id`, `finding_id`, `content`, optional
  `stage` / `kind`.

- [ ] **Step 1: Write SDL files** matching the fields above. Keep them
      addable via `node.add_schema` in tests (same style as
      `WebhookEvent` in `event_trigger_e2e.rs`).

- [ ] **Step 2: Write `experiments/README.md`** covering:
  - document-pipeline model and created-only constraint
  - how to apply an arm and kick with one mutation
  - non-goals (no barrier, no response-completed edges)
  - pointer to the design spec

- [ ] **Step 3: Commit**

```bash
git add experiments/
git commit -m "$(cat <<'EOF'
Add experiment seed schemas and experiments/ layout.

Document the EventTrigger document-pipeline model and the single GraphQL
seed write as the only kickoff.
EOF
)"
```

---

### Task 2: Desired-state arm — `single-loop`

**Files:**
- Create: `experiments/shapes/single-loop/` desired-state root (whatever
  `write_manifest_root` / export layout expects — mirror a real
  `gents config export` of a minimal agent, then slim)

**Shape:** One Task + one EventTrigger on `ExperimentJob` created → one
behavior. Tool selection: no orchestration, no subagent spawn. Prompt
template must include `{{ doc.job_id }}` and `{{ doc.prompt }}`.

- [ ] **Step 1: Export or hand-author** a minimal applyable root. Prefer
      exporting from a local init agent and editing down so field names match
      production validators (`desired_state/validate.rs`).

- [ ] **Step 2: Validate**

```bash
# against a running/local home as available
gents config validate --root experiments/shapes/single-loop
# or apply into an isolated --home and confirm Task + EventTrigger rows
```

- [ ] **Step 3: Commit**

```bash
git add experiments/shapes/single-loop
git commit -m "$(cat <<'EOF'
Add single-loop experiment arm as desired-state root.

One EventTrigger on ExperimentJob created fires one task — uniform kick
API with multi-node arms.
EOF
)"
```

---

### Task 3: Desired-state arm — `fanout-on-job`

**Files:**
- Create: `experiments/shapes/fanout-on-job/`

**Shape:** N≥2 Tasks (e.g. `search-a`, `search-b`, `search-c`) each with
its own EventTrigger on `ExperimentJob` / `created`. Same filter on seed
(or empty filter). Distinct prompts/behaviors so timeline participants
differ. `concurrency: parallel` on triggers unless serial is required for
mock stability.

- [ ] **Step 1: Author N tasks + N triggers** sharing seed collection;
      templates include `{{ doc.job_id }}`.

- [ ] **Step 2: Document expected_fires = N** in arm README or
      `harness.json` sidecar (see Task 5).

- [ ] **Step 3: Commit**

```bash
git add experiments/shapes/fanout-on-job
git commit -m "$(cat <<'EOF'
Add fanout-on-job experiment arm.

N EventTriggers on the same ExperimentJob create express fan-out without
fan_out_and_synthesize.
EOF
)"
```

---

### Task 4: Desired-state arm — `pipeline-two-stage`

**Files:**
- Create: `experiments/shapes/pipeline-two-stage/`

**Shape:**
- Stage 1: EventTrigger on `ExperimentJob` → task that is allowed to write
  `ExperimentFinding` (bounded write tool / tool selection as in
  `write_tool_trigger_e2e.rs` patterns)
- Stage 2: EventTrigger on `ExperimentFinding` → synthesizer/skeptic task

- [ ] **Step 1: Author both stages** with templates chaining `job_id`.

- [ ] **Step 2: Note in arm README** that CI will use mock write path;
      operator demos may inject findings via GraphQL if the model is weak.

- [ ] **Step 3: Commit**

```bash
git add experiments/shapes/pipeline-two-stage
git commit -m "$(cat <<'EOF'
Add pipeline-two-stage experiment arm.

Stage-1 writes ExperimentFinding docs; stage-2 EventTriggers fire on
those creates — pipeline edges without lifecycle updates.
EOF
)"
```

---

### Task 5: Operator harness — apply, kick, await, export

**Files:**
- Create: `experiments/harness/README.md`
- Create: `experiments/harness/kick.graphql` (templated mutation)
- Create: `experiments/harness/run.sh` (or `run.py` / small Rust bin —
  prefer shell + `gents` CLI if sufficient)
- Create: per-arm `experiments/shapes/<arm>/harness.json`:

```json
{
  "seed_collection": "ExperimentJob",
  "expected_trigger_ids": ["…"],
  "expected_min_requests": 1,
  "await_timeout_secs": 120
}
```

**Flow:**

1. `gents config apply --root experiments/shapes/$ARM --home $HOME`
2. Ensure schemas applied (document prerequisite; e2e registers SDL itself)
3. Render `kick.graphql` with `job_id`, `prompt`, `suite`, `arm`
4. POST single mutation to GraphQL
5. Poll AgentRequest where `caused_by_trigger_id` ∈ expected set **or**
   content contains `job_id`
6. On terminal / timeout: write `experiments/runs/$job_id/` with:
   - `meta.json` (arm, timestamps, request ids)
   - optional `gents trace timeline` / `project` dumps per request

- [ ] **Step 1: Implement kick + await** with clear failure messages.

- [ ] **Step 2: Dry-run** on mock or local demo agent if available;
      otherwise document required environment.

- [ ] **Step 3: Commit**

```bash
git add experiments/harness experiments/shapes/*/harness.json
git commit -m "$(cat <<'EOF'
Add experiment harness: apply, single GraphQL kick, await, export.

Kickoff is one seed create; measurement uses trigger lineage and optional
trace projection dumps.
EOF
)"
```

---

### Task 6: CI e2e — fan-out on seed create

**Files:**
- Create: `crates/gents/tests/e2e_triggers/experiment_graph_fanout_e2e.rs`
- Modify: `crates/gents/tests/e2e_triggers.rs` (mod path)

**Pattern:** Copy structure from `event_trigger_e2e.rs`:

1. `test_db` + register `ExperimentJob` SDL (inline or read from
   `experiments/schemas/` if path is stable in tests)
2. Mock endpoint + default behavior
3. `Gents::run`
4. Seed N Tasks + N EventTriggers (or apply from a fixture bundle if easy)
5. Wait snapshot ready + triggers active
6. **One** `create_ExperimentJob` mutation
7. Assert N AgentRequests with correct `caused_by_trigger_id` set and
   rendered content containing `job_id`
8. Assert EventTrigger `fire_count` / `last_status` bookkeeping

- [ ] **Step 1: Write failing test** (or green if fully self-contained).

- [ ] **Step 2: Run**

```bash
cargo test -p gents --test e2e_triggers experiment_graph_fanout -- --nocapture
```

- [ ] **Step 3: Commit**

```bash
git add crates/gents/tests/e2e_triggers.rs \
  crates/gents/tests/e2e_triggers/experiment_graph_fanout_e2e.rs
git commit -m "$(cat <<'EOF'
Add e2e: ExperimentJob create fans out to N EventTriggers.

Locks the single GraphQL kick → multi-request lineage contract for
experiment graphs.
EOF
)"
```

---

### Task 7: CI e2e — pipeline via write tool (stage-2 on finding create)

**Files:**
- Create: `crates/gents/tests/e2e_triggers/experiment_graph_pipeline_e2e.rs`
- Modify: `crates/gents/tests/e2e_triggers.rs`

**Pattern:** Combine `event_trigger_e2e` + `write_tool_trigger_e2e`:

1. Register `ExperimentJob` + `ExperimentFinding`
2. Stage-1 trigger on job; stage-2 on finding
3. Mock model returns a tool call that writes an `ExperimentFinding` (or
   drive `BoundedWriteTool` directly mid-test if simpler and still
   exercises the event bus)
4. Assert stage-2 request materializes with finding `doc` vars in content

- [ ] **Step 1: Implement and run**

```bash
cargo test -p gents --test e2e_triggers experiment_graph_pipeline -- --nocapture
```

- [ ] **Step 2: Commit**

```bash
git add crates/gents/tests/e2e_triggers.rs \
  crates/gents/tests/e2e_triggers/experiment_graph_pipeline_e2e.rs
git commit -m "$(cat <<'EOF'
Add e2e: pipeline stage-2 fires on ExperimentFinding create.

Proves multi-stage experiment graphs use new documents as edges, not
response lifecycle updates.
EOF
)"
```

---

### Task 8: Measurement helpers + docs cross-links

**Files:**
- Modify: `experiments/README.md` (measurement section)
- Modify: `experiments/harness/README.md` (timeline / project examples)
- Optional: small script `experiments/harness/export_metrics.sh` wrapping
  `gents trace timeline` + `project multi-agent --format eval-jsonl`
- Optional: link from `docs/gents.md` automation section (one paragraph +
  link) — only if maintainers want discoverability; keep the PR focused if
  unsure

- [ ] **Step 1: Document exact CLI invocations** and the metrics we trust
      (request count, wall time, fire_count) vs do not yet trust (full
      token rollup).

- [ ] **Step 2: Commit**

```bash
git add experiments/ docs/superpowers/
git commit -m "$(cat <<'EOF'
Document experiment measurement via timeline and multi-agent projection.
EOF
)"
```

---

### Task 9: Final verification

- [ ] **Step 1: Run e2e suite slices**

```bash
cargo test -p gents --test e2e_triggers experiment_graph -- --nocapture
```

- [ ] **Step 2: If any non-test Rust production code changed**

```bash
cargo check --workspace --all-targets
```

- [ ] **Step 3: Confirm** no secrets, no `experiments/runs/*` junk, plan
      checkboxes updated for completed tasks.

---

## Implementation PR sequencing (suggested Graphite / stack)

| PR | Contents |
| --- | --- |
| **This PR (docs)** | Design + plan only |
| PR A | Task 1–4: `experiments/` schemas + three arms |
| PR B | Task 5: operator harness |
| PR C | Tasks 6–7: e2e fan-out + pipeline |
| PR D | Task 8–9: measurement docs polish |

Alternatively A+B and C can land as two PRs if arms are small.

## Explicitly deferred

- `event_kind: updated` engine support
- Barrier / join trigger
- Live LLM quality A/B suite
- Promoting `ExperimentJob` into `gents-schemas` product schemas
- Cross-node P2P experiment arms (can clone `event_trigger_p2p_e2e`
  later)

## References

- Design: `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`
- E2E templates: `crates/gents/tests/e2e_triggers/event_trigger_e2e.rs`,
  `write_tool_trigger_e2e.rs`
- Desired state: `crates/gents-cli/src/desired_state/`
- Config CLI: `gents config apply|diff|export|validate`
- Trace CLI: `gents trace timeline|project`
