# Review demo page (design)

Company talk companion for the `demo/code-review` pack: a page that is already
open while we explain the graph, then hydrates a live document DAG when
`make review` seeds a `ReviewJob`.

Approved 2026-08-18 (layout A, left-rail talk track, three make targets).

## Problem

The pack already exercises collection tools, templated task prompts, and task
triggers — including grouped fan-in. `make review` already exists as a
`gents demo run` wrapper, but that path starts a fresh node, awaits the whole
graph, then **kills the server**. There is no talk surface.

The room needs:

1. A page that is live **before** any review is seeded.
2. A spoken walkthrough of what the pack is about to do, with exact folder
   names so a question can be answered by clicking the tree.
3. A short history of the three enabling features, with PR links.
4. A document DAG that appears as rows are written.
5. A session drawer that shows the live agent behind a clicked document.

## Talk choreography

Three terminals, in this order:

| Command | When | What |
| --- | --- | --- |
| `make review-page` | Before people sit down | Vite app on `:19190`. Empty DAG. Polls `:19191`. |
| `make review-serve` | Before the talk, or as "the node" | Durable home + `gents server --apply-root demo/code-review` on `:19191`. Stays up. |
| `make review` | The kick | Seeds one new `ReviewJob`. Does **not** start or stop the server. The page hydrates. |

`gents demo run demo/code-review` remains the unattended / CI path (await,
acceptance checks, kill server). The talk does not use it.

## Architecture

```text
browser  :19190          Vite host  apps/review-demo
   |  /healthz /sessions /api/v0/graphql
   v  (dev-server proxy)
gents server :19191      pack applied, durable home
   ^
   |  GraphQL create_ReviewJob
make review              seed only
```

The page never talks to Tauri. `gents-desktop-client` is a Tauri transport;
Operations/Chat panels that call `client.api.fetchRequestTimeline` are out of
reach without a bridge. Reuse is **tokens + UI primitives + CSS classes**.
Session rendering is page-owned, fed by GraphQL.

CORS is solved by the Vite proxy, not by opening the runtime to browser
origins.

## Make targets

Defaults stay `REVIEW_PORT=19191`, `REVIEW_ROOT=$(CURDIR)`, same
`GENTS_REVIEW_*` interpolation as today.

### `make review-serve`

1. Durable home: `demo/code-review/runs/demo-home` (gitignored via
   `demo/*/runs/`).
2. If the home has no identity, run the same `gents init` the pack runner uses
   (`write` tool package, `GENTS_REVIEW_ROOT`, backend/model from
   `experiment.json`).
3. Foreground: `gents server --home <home> --http-port $(REVIEW_PORT)
   --apply-root demo/code-review --p2p-transport none --no-codex-shim`.
4. Print `page http://127.0.0.1:19190` and `graphql http://127.0.0.1:19191/...`.
5. `REVIEW_RESET=1` deletes the home first.

Apply-time env (`GENTS_REVIEW_COORDINATOR_ENDPOINT`, model, context window, …)
is captured when the server starts. Seeding does not re-apply.

### `make review`

1. `GET http://127.0.0.1:$(REVIEW_PORT)/healthz` must succeed. If not:
   `start the pack node first: make review-serve`.
2. Allocate a unique `run_id` (`REVIEW_JOB_ID` or
   `review-YYYYMMDDTHHMMSSZ-<pid>`).
3. POST the same `create_ReviewJob` mutation `gents demo run` uses today
   (`run_id`, `focus`, `repository_path`, `base_ref`, `head_ref`,
   `lens_count`, `lens_min`, `lens_max`, `pr_number`), with
   `graphql::escape_graphql_string` (or the equivalent) on every interpolated
   value.
4. Print `seeded ReviewJob run_id=…` and exit 0. Do not await stages. Do not
   kill anything.

A new `run_id` creates new source rows, so first-seen EventTriggers fire again
on a reused home.

### `make review-page`

`npm --prefix apps/review-demo run dev` on port **19190**, `open` the URL.
Vite proxies:

| Browser path | Upstream |
| --- | --- |
| `/api/v0/graphql` | `http://127.0.0.1:${REVIEW_PORT}/api/v0/graphql` |
| `/healthz` | `…/healthz` |
| `/sessions` | `…/sessions` |
| `/status` | `…/status` |
| `/self` | `…/self` |

`REVIEW_PORT` is read at Vite startup (`process.env.REVIEW_PORT || 19191`).

## Page layout

Three columns, Source-dark semantic tokens.

```text
┌ status: Gents review · 19191 ready | waiting for ReviewJob ─────────┐
│ LEFT rail              │ CENTER (narrow) │ RIGHT                    │
│ What we’ll see         │ Live run DAG    │ Session                  │
│   4 write→trigger      │                 │ click a document         │
│ Enabling features      │                 │                          │
│   collection tools     │                 │                          │
│   templated prompts    │                 │                          │
│   task triggers        │                 │                          │
└────────────────────────┴─────────────────┴──────────────────────────┘
```

The center column is **narrower** than the rails. The DAG is a prop, not the
talk.

### What we’ll see

Copy:

> One seed write. Four document edges. No coordinator process.
> `make review` creates a `ReviewJob`; each create fires a trigger that
> materializes that stage’s Task on that stage’s Behavior.

Then four stacked edges. Ids **are** the folder names under
`demo/code-review/`.

| Write | Trigger | Behavior | Task | Tree |
| --- | --- | --- | --- | --- |
| seed `ReviewJob` | `review-recon` per_document | `review-recon` | `review-recon-task` | `schemas/review_job.graphql` · `event_triggers/review-recon/` · `agent-behaviors/review-recon/` · `tasks/review-recon-task/` |
| `write_review_area` → `ReviewArea` × N | `review-scan` parallel | `review-scan` | `review-scan-task` | `schemas/review_area.graphql` · `event_triggers/review-scan/` · `agent-behaviors/review-scan/` · `tasks/review-scan-task/` |
| `write_scan_result` → `ScanResult` × N | `review-verify` **per_group** | `review-verify` | `review-verify-task` | `schemas/scan_result.graphql` · `event_triggers/review-verify/` · `agent-behaviors/review-verify/` · `tasks/review-verify-task/` |
| `write_verification_summary` → `VerificationSummary` | `review-triage` per_document | `review-triage` | `review-triage-task` | `schemas/verification_summary.graphql` · `event_triggers/review-triage/` · `agent-behaviors/review-triage/` · `tasks/review-triage-task/` |

Side writes that are **not** trigger edges, shown as badges on the producing
node when present:

- scan: `write_candidate_finding` → `CandidateFinding`
- verify: `write_finding_verdict` → `FindingVerdict`
- triage: `write_finding` → `Finding`, `write_triage_report` → `TriageReport`

### Enabling features

Three cards. Each has: short definition, timeline with links, how it composes.

**Collection tools.** A `DatastoreToolSurface` grants named create/query tools
onto a collection. The model calls `write_review_area`; the runtime does one
validated write.

- Timeline: inline `write_tools` [#431](https://github.com/source-inc/gents/pull/431)
  (2026-06-08). Reusable surfaces [#1081](https://github.com/source-inc/gents/pull/1081)
  (2026-08-08). Query bindings 2026-08-17.
- Enables: each stage emits the next typed row. Those creates are the edges
  the other two features consume.

**Templated task prompts.** A Task’s `prompt.md` renders `{{ doc.* }}` and
`{{ event.* }}` from the source document. The row is the assignment.

- Timeline: landed 2026-04-22 with the trigger split —
  [#63](https://github.com/source-inc/gents/pull/63),
  sidecars [#67](https://github.com/source-inc/gents/issues/67),
  `args.*` [#70](https://github.com/source-inc/gents/pull/70). Then **mostly
  unused** until the August packs. Two later updates: cache-safe catalog /
  `node.*` / `ctx.now` in [#506](https://github.com/source-inc/gents/pull/506)
  (2026-06-15, spec [#497](https://github.com/source-inc/gents/issues/497)),
  and `group.*` in [#1113](https://github.com/source-inc/gents/pull/1113)
  (2026-08-13) so a fan-in prompt can dump the closed set. First pack use
  [#1081](https://github.com/source-inc/gents/pull/1081).
- Enables: a `ReviewArea` write becomes a self-contained scanner prompt.
  Verify uses `{{ group.docs }}` — that slot did not exist in April.

**Task triggers.** A Task is fired by a document: Schedule, Manual, or
EventTrigger on a collection create. Grouping is the latest increment, not
the whole feature.

- Timeline: designed 2026-04-21. Engine [#63](https://github.com/source-inc/gents/pull/63)
  + EventTrigger [#68](https://github.com/source-inc/gents/pull/68)
  + manual [#70](https://github.com/source-inc/gents/pull/70) (2026-04-22).
  First write→fire loop [#431](https://github.com/source-inc/gents/pull/431)
  (2026-06-08). Operator CLI [#474](https://github.com/source-inc/gents/pull/474).
  Filter validation [#1034](https://github.com/source-inc/gents/pull/1034) /
  [#1038](https://github.com/source-inc/gents/issues/1038) (2026-08-06).
  Pack graphs [#1081](https://github.com/source-inc/gents/pull/1081).
  Correlation + `per_group` designed
  [#1096](https://github.com/source-inc/gents/issues/1096), shipped
  [#1113](https://github.com/source-inc/gents/pull/1113) (2026-08-13).
- Enables: seed a `ReviewJob` and the graph runs itself. Surfaces write
  members; templates brief each worker; the trigger is the edge — and now
  the join.

## Live DAG

Nodes are **documents**, not sessions.

```text
ReviewJob
    │
    ├── ReviewArea (lens) ── CandidateFinding*
    │         └── ScanResult
    ├── ReviewArea …
    │
    └── (group closed)
            VerificationSummary
                FindingVerdict*   (on verify node)
                Finding*          (on triage node)
                TriageReport
```

Layout is **hand-laid by stage**, not force-directed:

1. Job
2. Fan-out of areas (label = `lens`), each with its scan result under it
3. Verify
4. Triage

Node states:

| State | When |
| --- | --- |
| expected | Stage not yet written; dashed |
| live | Document exists; request `lifecycle_state` is non-terminal |
| done | Document exists; request completed |
| failed | Request failed / error / timed out |
| waiting-group | ScanResults exist but `count < expected_total` |

Clicking a node selects it. The selected node’s `caused_by_source_doc_id`
request (or the request that **wrote** it, for produced rows) opens the
session drawer.

If several `ReviewJob` rows exist, watch the **newest** `run_id`. A small
status chip shows the id. No run picker in v1.

## Data flow

Poll:

- `GET /healthz` every 1s. Status bar: offline / ready / ready+run.
- When healthy, one GraphQL query every 1.5s for the newest job’s
  correlated rows plus requests:

```text
ReviewJob, ReviewArea, CandidateFinding, ScanResult,
FindingVerdict, VerificationSummary, Finding, TriageReport
  filter: run_id == <id>   (job list is unfiltered, then pick newest)

AgentRequest
  filter: caused_by_correlation == <id>
  fields: request_id, session_id, behavior_id, status, lifecycle_state,
          caused_by_trigger_id, caused_by_source_doc_id, created_at
```

Join:

| Document | Session request |
| --- | --- |
| `ReviewJob` | `caused_by_trigger_id == review-recon` |
| `ReviewArea` / `ScanResult` | `review-scan` whose `caused_by_source_doc_id` is that area (scan result shares the area’s scan request) |
| `VerificationSummary` / `FindingVerdict` | `review-verify` (one request per run) |
| `TriageReport` / `Finding` | `review-triage` |

A pure projector function `projectReviewGraph(snapshot) -> {nodes, edges}`
owns this. No React in that module.

When the selected request is set, a second poll (1s) loads that session:

- `AgentMessage` for `request_id` / `session_id`
- `AgentToolCall` for `request_id`
- latest assistant text (including streamed `AgentResponse` content if
  present)

The drawer shows: behavior id, lifecycle, token totals if available, tool
call list (name + status), and streaming assistant text. It uses
`gents-desktop-ui` buttons/chips and the tokens stylesheet. It does **not**
mount `ChatTranscriptPanel` (that type wants a Tauri
`DesktopSessionSnapshot`).

## Error handling

| Condition | UI |
| --- | --- |
| Page up, `:19191` down | Status `waiting for runtime` · empty dashed DAG · rails still readable |
| Runtime up, no `ReviewJob` | Status `ready · waiting for ReviewJob` |
| GraphQL error | Status `query failed` · last good snapshot kept |
| Seed while a run is mid-flight | New job becomes the watched run (newest `run_id`). Old nodes drop off the live graph. Do not try to show two runs. |
| `make review` with no server | Non-zero exit, message names `make review-serve` |
| Server process killed mid-talk | Page returns to `waiting for runtime` and freezes last snapshot until health returns |

No auth. Loopback only, same as the pack node.

## Host app

New workspace member `apps/review-demo` (same shape as `apps/fixture-host`,
**without** Tauri):

```text
apps/review-demo/
  package.json          @source-inc/review-demo-ui, private
  vite.config.ts        port 19190, proxy as above
  src/main.tsx
  src/App.tsx           three-pane shell
  src/styles.css        host token overrides after package imports
  src/talk/WhatWeWillSee.tsx
  src/talk/EnablingFeatures.tsx
  src/graph/projectReviewGraph.ts
  src/graph/ReviewDag.tsx
  src/live/pollRuntime.ts
  src/live/SessionDrawer.tsx
  src/live/types.ts
```

Dependencies: `react`, `react-dom`, `@source-inc/gents-desktop-tokens`,
`@source-inc/gents-desktop-ui`. Do not depend on
`gents-desktop-client` / chat / operations / fleet.

Root `package.json` workspaces already include `apps/*` via the two existing
apps; add `apps/review-demo` explicitly if the glob is not used (today the
list is explicit — add the entry).

Seed helper: `demo/code-review/scripts/seed-review-job.mjs`. Makefile
`review` invokes it. String escape must match `escape_graphql_string`
(backslash, then `"`).

## Testing

No live GLM. No browser e2e required for v1 (talk page, not product).

- Vitest on `projectReviewGraph`:
  - empty snapshot → expected skeleton
  - job only → recon live
  - N areas + partial scan results → waiting-group
  - full scan + verify + triage → done
  - newest `run_id` wins when two jobs are present
- One fixture JSON checked in under `apps/review-demo/src/graph/fixtures/`.
- Makefile help lists `review-page` and `review-serve`.

## Non-goals

- Changing `gents demo run` to leave the server up.
- A Tauri HTTP transport or reusing `RequestTracePanel` / `ChatTranscriptPanel`.
- Force-directed / pan-zoom graph libraries.
- Historical replay from `demo/code-review/runs/<job>/`.
- Editing documents from the page.
- Lean / conformance (presentation only).
- Showing two runs at once.

## Implementation notes (already true in this repo)

- `AgentRequest.caused_by_correlation` is the `run_id`.
- `demo run` always `server.start_kill()` even with `--keep-home`; keep-home
  only preserves files. Hence the persistent `review-serve` process.
- EventTriggers are first-seen on **document create**. A reused home is safe
  iff each kick writes a new `ReviewJob` with a new `run_id`.
- Empty Defra list literals must stay `null` if any seed helper grows arrays.
  Seed v1 has no arrays.
