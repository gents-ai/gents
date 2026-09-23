# Golden monitor: a fixed evaluation subject

`eval_monitor` is a complete, document-driven monitoring automation shipped as
a `documents` pack: one input schema, one `EventSource`, one enabled serial
`Trigger`, one `Task`, one `AgentBehavior`/`AgentContext` pair, one `Tools`
document and one `DatastoreToolSurface`.

It exists to be measured. The behavior's literal system prompt,
`agent_behaviors/eval_monitor/system_prompt.md`, is the **subject**: it is what
evaluation reads and what optimization later rewrites. Everything else in the
pack — the schema, the trigger chain, the tool grant, the mailbox surface — is
fixed scaffolding held constant so that a change in measured behavior is
attributable to the prompt and not to the configuration around it.

This pack is the fixed counterpart to the configurator evals, where a model
authors a monitor of its own in every trial. Here the monitor is authored once,
lives in the repository, and is reviewed like any other pack.

```text
create MonitorInput
        │
        ▼  EventSource + Trigger eval-monitor-input
   eval-monitor-task  ──list_mailbox_findings (read)──►  MailboxItem
                      ──file_mailbox_item   (write)──►  MailboxItem (one open row)
```

## Layout

| Path | Role |
| --- | --- |
| `schemas/monitor_input.graphql` | Pack-scoped SDL for `MonitorInput` — applied before the documents |
| `pack_config.json` | Canonical behavior, context, Tools, surface, Task, EventSource and Trigger |
| `agent_behaviors/eval_monitor/system_prompt.md` | **The subject.** The behavior's literal system prompt |
| `tasks/eval_monitor_task/prompt.md` | Prompt sidecar rendering the input into each request |

Evaluation cases and their inventory are added to this directory by separate
work and are deliberately **not** declared in `manifest.json`: only what the
manifest declares travels with the pack or enters its digest.

## Installation

```sh
gents pack show eval_monitor
gents pack install eval_monitor --home <initialized-home> --preview
gents pack install eval_monitor --home <initialized-home> \
  --inference-slot monitor=<existing-profile-id>
```

Installation applies `schemas/` first, so `MonitorInput` exists on the node
before the `EventSource` observes it, and then applies the documents as desired
state. Equivalent from a source checkout:

```sh
gents config validate --root packs/eval_monitor
gents config apply --root packs/eval_monitor --home <home> \
  --graphql http://127.0.0.1:19191/api/v0/graphql --bind-agent-did home
```

Triggers are `created`/first-seen only. A `MonitorInput` written before the
event source logs `event source now observing source collection` is seeded as
already seen and never fires; that is the one way to get a silent no-op.

## Bindings and prerequisites

- One inference slot, `monitor`, covering the single behavior `eval-monitor`.
  The pack authors no `InferenceBackend`, `InferenceProfile`, sampling,
  execution or retry document; the principal's existing inference owner remains
  the sole owner of model selection, and a missing or disabled binding fails
  before installation writes.
- `MailboxItem` is a built-in agent collection; the pack does not define it and
  does not define an output collection of its own.
- `GENTS_EVAL_WORKSPACE_ROOT` optionally selects the workspace root for the
  read-only file tools; the eval runner sets it per trial to the trial's
  workspace. Unset means `.`, the runtime's working directory.
- A fresh home, or at least a fresh principal, per run — see
  [Precondition: one fresh home per run](#precondition-one-fresh-home-per-run).
  The maintained mailbox row is scoped to the principal and is never closed.

## Tool and workspace authority

The `eval-monitor-tools` document grants exactly three things:

| Capability | Grant | Why |
| --- | --- | --- |
| `file_mailbox_item` | The canonical `MailboxItem` create declaration, with an explicit notification policy | The monitor's only output |
| `list_mailbox_findings` | A bounded query over `MailboxItem`, fixed projection, optional `status`/`kind` filters | The monitor must restate what it has already reported |
| File reads | `host.files.mode: "ReadOnly"` under `host.root` | Reading text an input points at |

There is **no** `host.bash` group: the monitor has no shell at all, not even a
read-only one, and cannot execute a host command by any route. It has no CLI
tools, no MCP services, no subagents and no self-configuration. The read-only
file grant is not machine inspection authority, and the prompt says so.

The mailbox surface derives from `packs/mailbox`'s `mailbox-writes` asset. A
document pack cannot depend by reference on an `assets` pack, so the
declaration is carried here, as `eval-monitor-mailbox`, with its own
notification policy and the added query entry.

`MailboxItem` may only ever be targeted by the canonical `file_mailbox_item`
declaration — tool name, collection, description and the three model-supplied
fields `title`, `summary` and `payload` are fixed by the runtime, and the
declaration must carry an explicit notification policy. Everything else on a
mailbox row (identity, `kind`, `action`, `source_kind`, `source_id`,
`requester_did`, `request_id`, provenance) is stamped by the runtime from that
policy and the current request. The model supplies findings, never routing.

## Inputs and outputs

**Input.** One `MonitorInput` document:

```graphql
mutation {
  create_MonitorInput(input: {
    correlation: "subject-1"
    message: "disk=81%, docker=unavailable"
  }) { _docID correlation }
}
```

`correlation` names the monitored subject; `message` describes its current
state. The task prompt renders both into the request; no reading is ever
hardcoded in the task.

The `MonitorInput` schema, `EventSource`, `Trigger` and `Task` are the
deployment path and are not exercised by evaluation: the eval runner submits
each stage's prompt directly as an `AgentRequest` in the trial's session — that
prompt being what the Task template would have rendered — and installs no
document that could fire the trigger.

**Output.** One open `MailboxItem`, maintained across inputs. The configured
notification policy uses a `condition` identity keyed `eval_monitor` with
`kind: "flag"` and `action: "ack"`, so repeated filings do not accumulate rows:
the first call creates the row and every later call replaces its `title`,
`summary` and `payload` in place. Identical content is reused rather than
rewritten. This is why the prompt requires each write to restate the complete
current picture.

The subject's output contract — what a reader may rely on — is:

- `title` is always exactly `Monitor findings`.
- `summary` is human-readable prose, one short line per finding.
- `payload` is a **JSON-encoded string**, not a nested object: the tool's
  parameter schema and the stored column are both `String`, so the model
  serialises the object and passes the text. Graders parse that text. Its shape
  is `{"version": 1, "findings": [{"correlation", "condition", "state", "detail"}]}`.
- `state` is `open` or `resolved`, and nothing else.
- There is at most one finding per `(correlation, condition)` pair. A cleared
  condition flips that finding to `resolved` and stays in the payload; a
  condition reported again flips it back to `open`.

A resolution is therefore a state transition inside the maintained record, not
a second mailbox row. The runtime's mailbox surface offers create and query
entries only — there is no update entry a model may hold — and a `condition`
identity is the mechanism by which one durable open row tracks a changing
situation.

### Precondition: one fresh home per run

The condition key is the fixed literal `eval_monitor`, and the pack never
closes the row it maintains. The row therefore has no bounded lifetime: it is
scoped to the principal, not to a run, and every input that principal ever
files accumulates in the same payload. The pack deliberately adds neither a
per-run key nor closing logic — a monitor that could retire its own record
would be reporting on itself.

So the convention only holds if the record starts empty. **Each run must use a
fresh home, or at least a fresh principal**, so that the first
`file_mailbox_item` call creates the row rather than inheriting one. Providing
that isolation is the runner's job, not the pack's; a reused home mixes earlier
findings into the payload and nothing in the pack will detect it.

## Completion and failure semantics

This section describes the deployment path, and the serial-skip hazard below is
specific to it; on the eval runner's direct-submission path there is no trigger
to skip, but stages are still submitted one at a time, because the mailbox
write is an unconditional read-then-replace of the single row and races the
same way (see below).

A `MonitorInput` create fires the trigger, which starts one `AgentRequest`
through the ordinary owned completion loop. Completion is that request reaching
a terminal `lifecycle_state` — the pack declares no goal, no output schema and
no task hooks, and the mailbox write is not itself a completion condition.

Concurrency is `serial`, and **serial does not mean queued**. An input created
while a prior request is still in flight is *skipped*, not deferred: the
trigger engine returns `Skipped { reason: "serial: prior fire still
in-flight" }`, the trigger observation records `last_status: "skipped"` with
that reason, and the document is already marked delivered — seen state is
committed before the fire result is known, so no rescan re-delivers it. The
input is dropped entirely, and only the trigger observation says so.

Serial is nonetheless the right setting here, because the alternative is
worse. The monitor's whole output is a single durable row that each write
replaces whole, and the write path has no compare-and-swap against what the
monitor read — `reuse_or_update` overwrites `title`, `summary` and `payload` on
the open row unconditionally. Under `parallel`, two overlapping requests would
interleave read-then-replace and silently drop one input's findings from the
record. Serial turns that silent corruption into a visible skip.

So the caller carries the pacing: **submit each input only after the previous
request has reached a terminal `lifecycle_state`.** Treat a
`last_status: "skipped"` on the trigger as a lost input and a defect in the
runner, not as work that will arrive later.

Failures stay visible through their existing owners rather than being repaired
here: an unusable inference binding fails at install; a missing collection
fails the event source; a malformed surface entry or an attempt to target
`MailboxItem` with anything but the canonical declaration fails closed when the
behavior's tool snapshot is built, before any request runs; a refused tool call
surfaces on the `AgentToolCall` row. Re-running is a new `MonitorInput`.

## Validation

```sh
node scripts/check_packs.mjs
cargo test -p gents pack::tests
gents config validate --root packs/eval_monitor
```

`pack::tests::all_packs_resolve_with_declared_assets_and_dependencies` resolves
this pack's manifest and digests every declared asset;
`every_configuration_pack_declares_slots_and_authors_no_inference_documents`
decodes `pack_config.json` through the shared loader and asserts the pack
declares slots and authors no inference documents.

Inspect a run end to end:

```sh
curl -s -X POST http://127.0.0.1:19191/api/v0/graphql -H 'content-type: application/json' \
  -d '{"query":"{ AgentRequest { caused_by_trigger_id lifecycle_state } MailboxItem { item_key status kind action title summary payload } }"}'
```

## Operational history

- Authored for M3 as the fixed monitor subject, replacing the per-trial monitor
  the configurator evals have a model author in each trial.
- The mailbox declaration was taken from `packs/mailbox` and brought up to the
  runtime's current canonical shape: the model-supplied fields are `title`,
  `summary` and `payload`, and an explicit notification policy is required. The
  asset in `packs/mailbox` still lists the older model-supplied field set.
- Not yet exercised against a live model endpoint from this pack. Evaluation
  does not drive the trigger chain; that chain mirrors the one the configurator
  evals author and drive end to end, and is the deployment path here.

## Declared topology

Document-trigger edges; task writes and host callbacks are described above.

<!-- pack-topology:start -->
```mermaid
flowchart LR
    n0["MonitorInput"]
    n1["eval-monitor-task"]
    n0 -->|"eval-monitor-input"| n1
```
<!-- pack-topology:end -->
