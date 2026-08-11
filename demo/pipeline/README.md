# Canonical pack: two-stage document pipeline

Self-contained Gents pack: domain SDL, `DatastoreToolSurface`, least-privilege
tool selections, tasks, and EventTriggers — all under this folder.

```text
create ExperimentJob
        │
        ▼  EventTrigger exp-stage1
   stage-1  ──write_experiment_finding (surface)──►  ExperimentFinding
                                                          │
                                                          ▼  EventTrigger exp-stage2
                                                     stage-2 (no tools)
```

## Layout

| Path | Role |
| --- | --- |
| `schemas/` | Pack-scoped SDL (`ExperimentJob`, `ExperimentFinding`) — applied by `config apply` |
| `datastore-tool-surfaces/experiment-writes/` | Create-tool grant → `ExperimentFinding` |
| `tool-selections/exp-tools-stage1/` | Links surface only; every other tool off |
| `tool-selections/exp-tools-stage2/` | Zero tools |
| `event_triggers/` | `exp-stage1` on job create; `exp-stage2` on finding create |
| `runs/` | Gitignored exports |

## Tools (least privilege)

| Behavior | Tools | Why |
| --- | --- | --- |
| stage-1 | `write_experiment_finding` only | Surface expands to one `BoundedWriteTool` create |
| stage-2 | none | Finding is already in the task prompt via `{{ doc.* }}` |

Surfaces name a **collection already on the node** (string). Pack apply
registers `schemas/` first so that name resolves.

## Run (anyone with a gents install + a model endpoint)

1. Init once (example uses DeepSeek V4 Flash on workstation-1):

   ```bash
   gents init --home <home> --inference-url http://100.73.235.38:8000/v1 \
     --backend-preset vllm --openai-wire-api chat-completions --model-name d4f \
     --tool-package minimal
   ```

2. **Validate + start server with pack apply** (recommended):

   ```bash
   gents config validate --root demo/pipeline
   gents server --home <home> --http-port 19191 --p2p-transport none --no-codex-shim \
     --apply-root demo/pipeline
   ```

   After ready, the server applies this pack against the **in-process** node
   (`schemas/` first, then desired-state; home DID rebind). The serving JSON
   includes an `apply_root` field with the apply report.

   Add `--apply-prune` only on a home dedicated to this pack: it makes the
   pack the complete desired state for that home's agent and deletes any
   config the pack does not declare (other behaviors, selections, skills,
   surfaces, and their reachable tasks/schedules/triggers).

   Equivalent without folding into server:

   ```bash
   gents server --home <home> --http-port 19191 --p2p-transport none --no-codex-shim
   gents config apply --root demo/pipeline --home <home> \
     --graphql http://127.0.0.1:19191/api/v0/graphql \
     --bind-agent-did home --force-rebind-concrete-did --prune
   ```

3. **Wait up to ~60s for the pack's backend to be probed, then for EventSource
   to observe the collections.** Both matter, in that order:

   The pack applies *after* the runtime is ready, so `exp-deepseek` is created
   after the backend prober's first cycle. Until the next cycle (60s interval)
   it sits at `probe_status: unknown` and both stage behaviors are reported
   unavailable — the serving JSON says `"status": "serving"` while this is
   true, so do not treat that as the go-signal. Watch for the reconcile that
   clears them:

   ```text
   runtime reconcile applied generation=3 ... proposed_unavailable_behavior_count=0
   event source now observing source collection source_collection=ExperimentJob
   ```

   Seeding before the observe log is the one way to get a silent no-op:
   triggers are `created`/first-seen only, so a doc written earlier is seeded
   as already-seen and never fires.

4. Kick:

   ```graphql
   mutation {
     create_ExperimentJob(input: {
       job_id: "exp-…"
       prompt: "Your research question"
       suite: "pipeline"
       arm: "pipeline"
     }) { _docID job_id }
   }
   ```

5. Await both stages, then export:

   ```bash
   curl -s -X POST http://127.0.0.1:19191/api/v0/graphql -H 'content-type: application/json' \
     -d '{"query":"{ AgentRequest { caused_by_trigger_id caused_by_trigger_kind lifecycle_state } }"}'
   ```

   A complete run has two `AgentRequest` rows — `exp-stage1` and `exp-stage2`,
   both `caused_by_trigger_kind: "event"`, both `completed` — plus one
   `ExperimentFinding` written by stage-1's surface tool. The automated runner
   also requires every provider request to pin a signed `AgentRequest` commit,
   then reconstructs the timeline and all four adapter projections from the
   persisted documents.

   ```bash
   gents trace timeline --request-id <id> --home <home>
   ```

   For cost, query `InferenceCall { prompt_tokens completion_tokens }` — not
   `AgentResponse.token_count`, which is a streaming word-count proxy.

### Verified run

Against DeepSeek V4 Flash on workstation-1, one seed produced: stage-1 fired
and called `write_experiment_finding`; the resulting `ExperimentFinding`
create fired stage-2; both requests reached `completed`; 3 inference calls,
1711 prompt + 437 completion tokens. Wall clock from seed to both stages
complete was a few seconds — the 60s backend probe is the only slow step, and
it happens once at startup.

## Design

- Topology: `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`
- Surfaces: `docs/superpowers/specs/2026-08-07-datastore-tool-surface-design.md`
