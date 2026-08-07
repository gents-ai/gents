# EventTrigger graph experiments

Config + docs only. Topology is expressed as **Tasks + EventTriggers** on
document creates — not `fan_out_and_synthesize`.

## Model

| Concept | Mapping |
| --- | --- |
| Node | Task bound to a behavior (prompt, model, tools) |
| Edge | EventTrigger with `event_kind: created` (first-seen only) |
| Shared state | Source document fields via `{{ doc.* }}` |
| Kickoff | Single GraphQL create of an `ExperimentJob` |
| Fan-out | Multiple EventTriggers on the same seed create |
| Pipeline | Stage agents create next-collection docs (e.g. `ExperimentFinding`) |

**v1 EventTrigger constraints**

- Only `created` / first-seen fires — never chain on `AgentResponse` or
  lifecycle updates.
- No barrier / fan-in primitive (concurrency is per-trigger).
- Stage edges require **new documents** in a watched collection.

## Arms

| Arm | Topology | Expected fires |
| --- | --- | --- |
| `shapes/single-loop` | One trigger on `ExperimentJob` → one task | 1 request |
| `shapes/fanout-on-job` | Three triggers on the same `ExperimentJob` create | 3 requests |
| `shapes/pipeline-two-stage` | Stage-1 on job; stage-2 on `ExperimentFinding` create | ≥1 stage-1 (+ stage-2 after finding write) |

All arms use **DeepSeek V4 Flash** (`d4f`) via OpenAI-compatible chat
completions at `http://100.73.235.38:8000/v1` (Tailscale peer
`workstation-1`). Tool selections set `orchestration_enabled: false`.

## Schemas

Register before kicking (custom collections are not in product schemas):

- `schemas/experiment_job.graphql` — seed collection
- `schemas/experiment_finding.graphql` — pipeline stage-2 source

Fields on `ExperimentJob`: `job_id`, `prompt`, `suite`, `arm`.

## Operator flow

Static check (no server):

```bash
gents config validate --root experiments/shapes/<arm>
```

Live path that has been exercised end-to-end against DeepSeek V4 Flash
(`d4f` on workstation-1):

1. **Init a home** (once) pointed at the same backend the arms use:

   ```bash
   gents init --home <home> --inference-url http://100.73.235.38:8000/v1 \
     --backend-preset vllm --openai-wire-api chat-completions --model-name d4f \
     --tool-package minimal
   ```

2. **Register experiment SDL via local home** (GraphQL remote schema apply
   may return “Collection management operations are not enabled”):

   ```bash
   # stop the server if it is holding the store open
   gents schema apply experiments/schemas --home <home>
   ```

3. **Start the server**, then **apply the arm** (checked-in DIDs are
   placeholders — rebind is required):

   ```bash
   gents server --home <home> --http-port 19191 --p2p-transport none --no-codex-shim
   gents config apply --root experiments/shapes/<arm> --home <home> \
     --graphql http://127.0.0.1:19191/api/v0/graphql \
     --bind-agent-did home --force-rebind-concrete-did
   ```

4. **Wait until the event source observes the seed collection.** Logs should
   show `event source now observing source collection
   source_collection=ExperimentJob` *before* you create a job. Seeds created
   earlier are treated as already-seen (v1 created/first-seen only) and will
   **not** fire.

5. **Kick with one seed create** (fresh `job_id` each run):

   ```graphql
   mutation {
     create_ExperimentJob(input: {
       job_id: "exp-…"
       prompt: "Your research question"
       suite: "topology-ab"
       arm: "single-loop"
     }) { _docID job_id }
   }
   ```

6. **Await lineage and export**

   Poll:

   ```graphql
   {
     AgentRequest(filter: { caused_by_trigger_id: { _eq: "exp-single" } }) {
       request_id
       content
       caused_by_trigger_id
       caused_by_trigger_kind
       lifecycle_state
     }
   }
   ```

   Trigger ids: `exp-single`; `exp-fan-a` / `exp-fan-b` / `exp-fan-c`;
   `exp-stage1` / `exp-stage2`.

   Then:

   ```bash
   gents trace timeline --request-id <id> --home <home>
   gents trace project --projection multi-agent --format eval-jsonl \
     --request-id <id> --home <home>
   # token cost (not on the timeline row today):
   # InferenceCall(filter: { request_id: { _eq: "…" } }) {
   #   prompt_tokens completion_tokens cached_input_tokens
   # }
   ```

   Drop exports under `runs/<job_id>/` (gitignored).

## Measurement

Trust for cost/structure:

- Request count / siblings by `caused_by_trigger_id`
- Inference call count and wall time from the run timeline
- **Token usage from `InferenceCall.prompt_tokens` /
  `completion_tokens` / `cached_input_tokens`** — query `InferenceCall` by
  `request_id` (timeline rows do not project these fields today)

Do **not** use `AgentResponse.token_count` as a cost metric — it is a
streaming word-count proxy and can read 0 on recovered responses.

Quality scoring (LLM-as-judge, human rubrics) is out of band via
eval-jsonl export.

## Scope of this tree

Shipped here: **desired-state arms + schemas + this operator guide**.  
Not in this tree or PR: CI e2e (`experiment_graph_e2e`),
`cli_experiment_shapes`, or a custom harness binary. Runtime already
provides config apply, schema apply, triggers, and `gents trace`.

## Design

See `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`
and the status plan under
`docs/superpowers/plans/2026-08-07-event-trigger-graph-experiments.md`.
