# Experiment pipeline (EventTrigger document graph)

One productionized arm: a **two-stage document pipeline** driven only by
EventTriggers on create (not `fan_out_and_synthesize`).

```text
create ExperimentJob
        │
        ▼  EventTrigger exp-stage1
   stage-1 behavior  ──write_experiment_finding──►  ExperimentFinding
        │                                              │
        │                                              ▼  EventTrigger exp-stage2
        │                                         stage-2 behavior (no tools)
```

Stage-1’s only model-visible tool is **`write_experiment_finding`**, granted
via the apply-owned `DatastoreToolSurface` `experiment-writes` (not inline
`write_tools`). Stage-2 needs no tools: the finding is already in the task
prompt via `{{ doc.* }}`.

## Layout

```text
experiments/                         desired-state root (this directory)
  schemas/                           ExperimentJob + ExperimentFinding SDL
  datastore-tool-surfaces/
    experiment-writes/               WriteToolDecl surface → ExperimentFinding
  tool-selections/
    exp-tools-stage1/                surface link only; all other tools off
    exp-tools-stage2/                zero tools
  agent-behaviors|tasks|event_triggers|…
  runs/                              gitignored exports
```

## Tool surface (least privilege)

| Behavior | Tools advertised | Why |
| --- | --- | --- |
| `exp-stage1` | `write_experiment_finding` only | Advance the graph by creating a finding |
| `exp-stage2` | *(none)* | Finding fields are in the prompt; synthesize text only |

Explicitly off on both selections (defaults that would otherwise leak tools):

- `enable_file_tools` / `enable_bash` / `enable_meta_tools`
- `enable_context_budget` (version-gated default-true)
- `enable_defra_query` / memory / session history / self-config
- orchestration / subagent spawn / MCP

Job and finding text arrive through EventTrigger templates (`{{ doc.* }}`), so
stage agents do not need datastore query tools for this arm.

## Schemas

Custom collections (not product baseline) — apply once per home:

- `schemas/experiment_job.graphql` — seed
- `schemas/experiment_finding.graphql` — stage edge

## Operator flow

Static check (no server):

```bash
gents config validate --root experiments
```

Live path (DeepSeek V4 Flash `d4f` on Tailscale `workstation-1` is the
checked-in backend example):

1. **Init a home** (once):

   ```bash
   gents init --home <home> --inference-url http://100.73.235.38:8000/v1 \
     --backend-preset vllm --openai-wire-api chat-completions --model-name d4f \
     --tool-package minimal
   ```

2. **Register experiment SDL** (stop the server if it holds the store open):

   ```bash
   gents schema apply experiments/schemas --home <home>
   ```

3. **Start server**, then **apply this tree** (placeholders rebind to home DID):

   ```bash
   gents server --home <home> --http-port 19191 --p2p-transport none --no-codex-shim
   gents config apply --root experiments --home <home> \
     --graphql http://127.0.0.1:19191/api/v0/graphql \
     --bind-agent-did home --force-rebind-concrete-did --prune
   ```

4. **Wait** until logs show EventSource observing `ExperimentJob` **before**
   creating a seed (v1 is created/first-seen only; earlier seeds never fire).

5. **Kick** with a fresh `job_id`:

   ```graphql
   mutation {
     create_ExperimentJob(input: {
       job_id: "exp-…"
       prompt: "Your research question"
       suite: "pipeline"
       arm: "pipeline-two-stage"
     }) { _docID job_id }
   }
   ```

6. **Await lineage** (`exp-stage1`, then `exp-stage2` after the finding write):

   ```graphql
   {
     AgentRequest(filter: { caused_by_trigger_id: { _eq: "exp-stage1" } }) {
       request_id lifecycle_state caused_by_trigger_id
     }
   }
   ```

   ```bash
   gents trace timeline --request-id <id> --home <home>
   # tokens: query InferenceCall by request_id (not AgentResponse.token_count)
   ```

   Drop exports under `runs/<job_id>/` (gitignored).

## Measurement

- Request count / stage by `caused_by_trigger_id`
- Timeline wall time; **tokens from `InferenceCall`**
- Do **not** use `AgentResponse.token_count` as cost

## Design

- Topology: `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`
- Create-tool surface: `docs/superpowers/specs/2026-08-07-datastore-tool-surface-design.md`
