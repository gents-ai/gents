# R4c Agent-Facing Background Work Management Tools Design

Status: draft for Jack approval, revision 1
Date: 2026-05-14
Tracking: TBD (suggested title: "R4c: agent-facing background work management")
Refs: R4 (#177), R6 (this branch), #191, #189, #190, #184, #160

Depends on:

- `docs/superpowers/specs/2026-05-12-r4-agent-facing-tools-design.md` (R4 spawn/wait/cancel surface and the originally-deferred R4c sketches)
- `docs/superpowers/specs/2026-05-14-tool-backgrounding-design.md` (R6 parametric `BackgroundedKind`, rename of `Proofs/Subagent/` → `Proofs/Background/`, `<tool-completion>` transcript element)
- `docs/superpowers/plans/2026-05-14-tool-backgrounding.md` (R6 implementation plan, including the Task 0 rename)

## Goal

R4c adds five agent-facing tools that let a parent agent observe and steer the
background work it spawned. The five tools split per `BackgroundedKind`:

- **Subagent kind:** `list_subagents`, `read_subagent_transcript`, `steer_subagent`
- **Tool kind:** `list_background_tools`, `read_tool_output`

There is no unified `list_background_work` / `read_background_transcript`. The
envelopes differ per kind, the LLM gets typed tool names, and we don't pay for
a `kind` discriminator field.

R4c was originally deferred by R4 with "first-90-days won't need them." Jack
has now reversed that. The design is sized as small as honestly possible:
these are management tools, not new state machines.

## Strategic Claim

R4c adds **zero new Lean transitions** and **one new theorem** (B5
preservation under the `steer_subagent(interrupt=true)` composition, see
"Verified Obligations"). Every primitive R4c uses already exists in the
verified model:

- the parent-child edge for authorization (R4)
- `Proofs/Background/*` bridge rows for listing (R6 parametric `BridgedState`)
- `Proofs/Transcript/*` rows for transcript rendering (#191)
- `Proofs/Session/*` queue (`QueueSource.steering` variant already present) for
  steering append semantics (R4a)
- `interrupt` (in `Proofs/Request/Transition.lean`) +
  `bridge_cancel_cascade` (R6 parametric) + `pendingAfterDrain` for
  `steer_subagent(interrupt=true)`

R4c is glue plus rendering. The Lean obligation is composition correctness,
not new modules. Conformance witnesses (Section "Verified Obligations") cover
the six observable shapes Rust must not drift on.

## Scope And Sequencing

R4c sequences after R6 merges to main. Concretely:

1. **R6 prerequisite (kickoff gate):** R6 merged to main. R6 Task 0's rename
   (`Proofs/Subagent/` → `Proofs/Background/`, `subagent_completion.rs` →
   `background_completion.rs`, `subagent_tools.rs` → `background_tools.rs`)
   must be on `main` before R4c implementation begins. R4c imports
   `Proofs.Background.*` and edits `background_tools.rs`.
2. **Rebase on merge:** when R6 merges, R4c rebases. Conflicts expected in
   `background_tools.rs` (R4c adds five new tool entries to a file R6 will
   have substantively touched), `hook.rs` and `hook/persistence.rs`
   (interceptor wiring), and `tool_surface/selection.rs` (registration gates).
3. **R5 sibling:** R5 brainstorms cross-deployment in parallel. R4c and R5
   share no file surface. If R5 lands an `R5/Cross/` proof module that lists
   remote work, R4c's v2 cross-deployment lift folds it in as a pure additive
   change to the same five tool envelopes.
4. **PR shape:** R4c is one PR with substantive commits per task. No "pure
   rename" no-op commit (R6 owns that).

### What R4c reuses (unchanged)

- The parametric bridge transitions (R6 — `Proofs/Background/*`).
- The session queue / coalesced wake-up infrastructure (R4a).
- The transcript model and dedupe (#191).
- The R4 hook-intercepted no-`AgentToolCall`-row pattern (carried forward
  from `wait_subagent` / `cancel_subagent`).
- Authorization via the parent-child edge (R4 + R6).

### What R4c adds (new surface only)

- Five new tool definitions in `background_tools.rs`.
- A pure-function transcript renderer in `background_tools/transcript_render.rs`.
- Hook interceptor entries for the five new tools.
- Conformance witnesses in `Proofs/Conformance/Contracts.lean` (no new Lean
  modules — additions to the existing contract surface).
- Registration gate wiring against existing `ToolSelectionDocument` fields.

## Tool Surface

All five tools register as native Rig tools and are hook-intercepted before
ordinary `AgentToolCall` persistence. None of the five writes a parent-side
tool-call row.

### Registration gates

- `list_subagents`: registers when behavior has at least one
  `subagent_targets` entry. (Discovery surface for spawn; same gate as
  `spawn_subagent`.)
- `read_subagent_transcript`: requires `subagent_targets` non-empty AND
  `subagent_steering_enabled = true`.
- `steer_subagent`: requires `subagent_targets` non-empty AND
  `subagent_steering_enabled = true`. Additionally requires
  `subagent_background_enabled = true` because steering is only meaningful
  for backgrounded children.
- `list_background_tools`: registers when behavior's
  `backgroundable_tool_names` is non-empty. (Discovery surface for
  `background_tool`; same gate as R6's three tool-kind tools.)
- `read_tool_output`: same gate as `list_background_tools`. R6 noted this
  tool would land bundled with R4c.

### `list_subagents`

Arguments:

```json
{
  "status": "running",
  "limit": 20
}
```

Defaults: `status = "running"`, `limit = 20`. `status` accepts
`"running" | "terminal" | "all"`. `limit` hard cap is 50.

Returns an array of compact entries reachable from **this parent request's**
authorized subagent edges:

```json
{
  "read_at": "2026-05-14T17:30:00Z",
  "truncated": false,
  "entries": [
    {
      "child_request_id": "...",
      "child_session_id": "...",
      "behavior_id": "...",
      "deployment_id": "...",
      "await_mode": "background",
      "status": "running",
      "created_at": "...",
      "last_update": "...",
      "depth": 1
    }
  ]
}
```

`status` values match the R2 `ChildTerminal` vocabulary plus `"running"`:
`"running" | "completed" | "failed" | "dead" | "interrupted" | "superseded"`.

`deployment_id` carries the local deployment in v1. The field is reserved so
the v2 cross-deployment lift is additive.

### `list_background_tools`

Arguments:

```json
{
  "status": "running",
  "limit": 20
}
```

Same defaults and caps as `list_subagents`.

Returns:

```json
{
  "read_at": "...",
  "truncated": false,
  "entries": [
    {
      "tool_call_id": "...",
      "tool_name": "bash",
      "deployment_id": "...",
      "await_mode": "background",
      "status": "running",
      "created_at": "...",
      "last_update": "...",
      "stdout_bytes": 12345,
      "stderr_bytes": 678
    }
  ]
}
```

`status` values: `"running" | "completed" | "failed" | "cancelled" | "interrupted"`.

`stdout_bytes` / `stderr_bytes` are the ring buffer's current occupancy for
running rows and the persisted captured payload size for terminal rows.

### `read_subagent_transcript`

Arguments:

```json
{
  "child_request_id": "...",
  "since_sequence": 0,
  "limit": 20,
  "max_chars": 6000,
  "include_user_messages": false,
  "include_tool_results": false
}
```

Defaults: `since_sequence = 0`, `limit = 20`, `max_chars = 6000`,
`include_user_messages = false`, `include_tool_results = false`.

Hard caps: `limit ≤ 100`, `max_chars ≤ 24000`, per-tool-result body snippet
≤ 256 bytes (the rendered snippet, not the underlying row).

Returns:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "from_sequence": 1,
  "through_sequence": 12,
  "next_sequence": 13,
  "truncated": false,
  "transcript": "[assistant seq=1]\n...\n[assistant seq=4]\n..."
}
```

`next_sequence = last_included_sequence + 1` so a follow-up call with
`since_sequence = next_sequence` is correct.

`truncated = true` iff rendering stopped before consuming all matching rows
because `limit` or `max_chars` was hit.

Render rules (Section "`read_subagent_transcript` Render Rules") detail what
goes into the `transcript` blob.

### `read_tool_output`

Arguments:

```json
{
  "tool_call_id": "...",
  "max_bytes_per_stream": 16384
}
```

Defaults: `max_bytes_per_stream = 16384`. Hard cap: 262144 (256 KB).

Returns:

```json
{
  "tool_call_id": "...",
  "tool_name": "bash",
  "status": "running",
  "stdout": {
    "bytes": "...",
    "truncated": false,
    "total_bytes_seen": 12345
  },
  "stderr": {
    "bytes": "...",
    "truncated": false,
    "total_bytes_seen": 678
  },
  "exit_code": null
}
```

`exit_code` is `null` when `status = "running"`; otherwise the captured exit
code for tools that produce one (bash) or `null` for tools that don't (MCP).

Source dispatch by row state:

- **Running:** in-memory ring buffer maintained by R6 (256 KB per stream, see
  R6 §"Streaming Output Behavior").
- **Terminal:** the persisted `<tool-completion>` payload R6 writes on bridge
  projection.

Caller-facing semantics are identical regardless of state.

`stdout.bytes` is the **tail** of the available buffer — the most recent
`max_bytes_per_stream` bytes. Best-effort UTF-8 cleaning trims partial
multibyte sequences at the head/tail. Binary streams may have non-printable
characters in the cleaned tail.

`truncated = true` iff older bytes were dropped (running: ring buffer
overflowed; terminal: R6's terminal envelope already had `truncated = true`).

`total_bytes_seen` is monotonic: it counts every byte that has flowed
through the stream, including bytes the ring buffer subsequently dropped.
Use it to detect "still making progress".

### Tail-only snapshot limitation

The snapshot posture has a real cost callers must understand: an agent
polling `read_tool_output` every N seconds **will miss any line the tool
emits that gets pushed out of the ring buffer between polls**. Concretely,
if a bash subprocess produces 300 KB of stdout between two polls and the
buffer is 256 KB, ~44 KB of intermediate output is gone — the agent sees
the most recent 256 KB only.

This is the right tradeoff for v1: the common case is "what is bash doing
right now?", not "give me the complete log." `total_bytes_seen` lets the
caller detect that bytes were dropped (`total_bytes_seen > buffer_size`
implies older bytes are gone), but cannot recover them.

If a future caller needs lossless tail-following, v2 may add a
`since_byte` incremental protocol or persist the full stream to disk; both
are out of scope for v1. Callers that need the full output today should
not background the tool — they should run it foreground.

### `steer_subagent`

Arguments:

```json
{
  "child_request_id": "...",
  "message": "...",
  "interrupt": false
}
```

Default: `interrupt = false`.

Returns (success):

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "queued_request_id": "...",
  "interrupted_active_request_id": null,
  "drained_wake_up_request_ids": []
}
```

`interrupted_active_request_id` is present (non-null) iff `interrupt = true`
and there was an active child request to interrupt.

`drained_wake_up_request_ids` is the list of automated wake-up `AgentRequest`
ids terminalized as part of the `interrupt = true` drain.

Two operating modes detailed in Section "`steer_subagent` Semantics" below.

## Authorization

Authorization is per-parent-request lineage for all five tools.

### Operand lineage check

- **Subagent operand (`child_request_id`):** must satisfy
  `AgentRequest.caused_by_parent_request_id = caller.request_id`. R4's
  spawn/wait/cancel already authorize through this; R4c reuses verbatim.
- **Tool operand (`tool_call_id`):** must satisfy
  `AgentToolCall.request_id = caller.request_id` AND
  `await_mode = .background`. R6's `wait_tool`/`cancel_tool` already check
  this; R4c reuses verbatim.

No tool re-authorizes against `subagent_targets` or
`backgroundable_tool_names` — the row's existence is the authorization
witness. R6's pattern for the wait/cancel surface carries forward.

### Listing scope

`list_subagents` and `list_background_tools` scope to the caller's parent
request lineage:

- `list_subagents` returns rows with
  `AgentRequest.caused_by_parent_request_id = caller.request_id`.
- `list_background_tools` returns rows with
  `AgentToolCall.request_id = caller.request_id` AND
  `await_mode = .background`.

A reissued parent request (same session, new `request_id`) does **not** see
the prior request's children. This is the strictest least-privilege scope and
matches the parent-child edge that R4 already authorizes through.

The `status` filter (`"running" | "terminal" | "all"`) further selects within
the lineage scope.

### Failure shapes

All five tools use the structured error envelope R4 established:

```json
{
  "ok": false,
  "failure_class": "tool_not_allowed",
  "path": "/child_request_id",
  "message": "child not owned by this parent request",
  "retryable": false,
  "service_id": "background_management",
  "tool_name": "read_subagent_transcript"
}
```

Persisted lifecycle failure class:

- `FailureClass::ServiceUnavailable` for authorization failures
  (consistent with R4 and R6).
- `FailureClass::ArgumentInvalid` for malformed arguments and for
  state-mismatch rejections.

### Specific authorization failures

- `child_request_id` not owned by caller → `tool_not_allowed`,
  `"child not owned by this parent request"`.
- `tool_call_id` not owned by caller → `tool_not_allowed`, same shape.
- `tool_call_id` is not backgrounded (foreground row) →
  `argument_invalid`, `"tool call is not backgrounded"`. An agent that calls
  `read_tool_output` on an ordinary `read_file` row hits this.
- `steer_subagent` called on terminal child → `argument_invalid`,
  `"child is in terminal state '<x>'; spawn a new subagent instead"`.
- `steer_subagent` called on foreground child → `tool_not_allowed`,
  `"foreground subagents cannot be steered; call cancel_subagent first"`.
  This closes the loophole if hooks reorder somehow; in practice the parent
  is blocked inside `spawn_subagent`/`wait_subagent` and cannot call
  `steer_subagent`.
- `read_subagent_transcript` `since_sequence` exceeds child session's
  current `max_sequence` → returns empty `transcript` with
  `from_sequence = through_sequence = next_sequence = since_sequence`,
  `truncated = false`. Not an error; the agent has caught up.

### Recovery-time validation

R4c tools are pure read/observe/steer surfaces. There is no row to
terminalize at recovery — these tools don't write authorization-bound state.
No recovery sweep changes.

## `read_subagent_transcript` Render Rules

The `transcript` field is a single text blob, never a structured array. Each
rendered message is one line-prefixed block.

### Default (assistant-only) shape

```
[assistant seq=4]
I'll start by reading the config file to see the current backend.

[assistant seq=7]
The config has three backends. I'll bench all three.
```

Blocks are separated by a blank line.

### `include_user_messages = true`

User messages interleave with `[user seq=N]` prefix:

```
[user seq=6]
make sure you also check the staging config

[assistant seq=7]
...
```

### `include_tool_results = true`

Tool-result rows render as snippets capped at 256 bytes:

```
[tool-result seq=8 tool=bash]
... up to 256 bytes of stdout/stderr summary ...
```

Tool-call XML envelopes from assistant messages are **never** rendered.
Assistant messages that emitted tool calls render with a compact suffix:

```
[assistant seq=7 tool_calls=2]
I'll check the config and then run the benchmark.
```

### Bridge call hiding

Bridge `AgentToolCall` rows (Subagent kind and Tool kind both) are always
hidden from this view. The parent doesn't need to see "child spawned
subagent X" or "child backgrounded bash Y" in this transcript — it's the
child's internal mechanism, and listing it would mislead the parent into
trying to operate on rows it doesn't own. The bridge row renders as nothing.

### Filtering pipeline

Order of operations:

1. Query `AgentMessage` rows where `session_id = child_session_id` AND
   `sequence > since_sequence`, ordered by `sequence` ascending.
2. Drop tool-call/tool-result message rows unless
   `include_tool_results = true`.
3. Drop `user`-role rows unless `include_user_messages = true`.
4. Drop bridge call references (`AgentMessage` rows that name a bridge
   `AgentToolCall`).
5. Accumulate rendered blocks until the next block would exceed `limit`
   messages OR `max_chars` total characters.
6. Emit the envelope with `from_sequence` = first rendered row's sequence,
   `through_sequence` = last rendered row's sequence,
   `next_sequence = through_sequence + 1`,
   `truncated = (rows_remaining > 0)`.

Compaction (#184) running concurrently could reshape history between calls.
The `next_sequence` cursor stays valid because compaction preserves sequence
numbers.

## `read_tool_output` Buffer Source

Source dispatch by row state, restated for completeness:

- **Running:** in-memory ring buffer keyed by `tool_call_id`, maintained by
  R6's executor (R6 §"Streaming Output Behavior"). The buffer holds the most
  recent 256 KB per stream; older bytes are dropped on overflow.
- **Terminal:** the persisted `<tool-completion>` payload R6's projector
  writes (R6 §"Streaming Output Behavior"). The payload carries the same
  byte cap and `truncated` flag.

The caller doesn't distinguish. R4c's hook reads from whichever source the
row state selects.

### Concurrency

For running rows, R4c locks the ring buffer for the duration of the snapshot
(already done by R6's buffer; R4c reuses the existing read lock). For
terminal rows, the persisted payload is immutable.

### Best-effort UTF-8

The ring buffer is byte-oriented. On read, R4c trims partial multibyte
sequences at the head and tail of the returned slice. A stream that is
fundamentally binary produces a cleaned tail with non-printable bytes
preserved within whole UTF-8 sequences only. This is the right tradeoff for
the common case (bash producing text output); binary streams should not be
backgrounded in practice.

## `steer_subagent` Semantics

Both modes compose existing primitives. Zero new Lean transitions.

### `interrupt = false` (append)

1. Resolve `child_request_id` through the parent-child edge (Section
   "Authorization").
2. Refuse if the child is in any terminal state (`argument_invalid`).
3. Refuse if the child is foreground (`tool_not_allowed`).
4. Append a user-role `AgentMessage` row to the child session transcript
   with the steering `message` as its content. This makes the steering
   message durable model-visible context, symmetric with how ordinary user
   input lands in a session.
5. Compose a new `AgentRequest` row in the child session:
   - `session_id` = child session
   - `behavior_id` = child behavior (preserved)
   - `subagent_depth` = child's depth (preserved)
   - `caused_by_parent_request_id` = caller's `request_id` (the steering
     parent), preserving lineage
   - `caused_by_parent_tool_call_id` = `null` (no bridge edge; this is a
     session-queue append, not a spawn)
   - `metadata.queue.source = "steering"`
   - `metadata.queue.policy = "append"`
   - prompt = the steering `message`
6. Return the envelope with `queued_request_id` set to the new request id;
   `interrupted_active_request_id` = `null`;
   `drained_wake_up_request_ids` = `[]`.

The child claims the steering request when its current active request
terminalizes (per R4a queue semantics).

### `interrupt = true` (replace)

1. Resolve and refuse-if-terminal-or-foreground as above.
2. Fire the existing **request interrupt** transition on the child's active
   `AgentRequest`. This is the same `interrupt` transition that user-cancel
   uses (`Proofs/Request/Transition.lean`).
3. The interrupt cascades through any live tool/subagent edges on the
   interrupted request via the existing R6-parametric
   `bridge_cancel_cascade`. Same cascade vector as user-cancel of the
   child's active request.
4. Drain pending automated wake-ups in the child session: call
   `pendingAfterDrain` with `source = .subagentCompletion` (post-R6 alias:
   `.backgroundCompletion`) and `queueKey =
   subagent_completion:<child_session_id>` (post-R6 alias:
   `background_completion:<child_session_id>`). The drained wake-ups are
   terminalized into the queue's `terminal` set per R4a; rows are not
   deleted.
5. Append a user-role `AgentMessage` to the child session transcript with
   the steering `message` (same as step 4 of `interrupt = false`).
6. Compose a new `AgentRequest` row in the child session (same fields as
   step 5 of `interrupt = false`), plus one extra metadata field:
   `metadata.queue.interrupted_request_id = <interrupted_active_request_id>`
   for trace clarity.
7. Return the envelope with `queued_request_id`,
   `interrupted_active_request_id` set to the interrupted active request's
   id, and `drained_wake_up_request_ids` set to the list of terminalized
   wake-up ids.

### Invariant preservation by reuse

Because `interrupt = true` uses the existing `interrupt` transition, the
existing `bridge_cancel_cascade`, the existing `pendingAfterDrain`, and an
ordinary `AgentRequest` append, every B-theorem on `Proofs/Background/*`
and every S-theorem on `Proofs/Request/*` carries through. The Lean
obligation is composition correctness, not a new transition.

The implementation plan may optionally add a derived property
`steer_with_interrupt_preserves_link_symmetry` discharged from existing B5
and the interrupt transition's invariants. Belt-and-suspenders; not strictly
required.

### Why no transcript element for the steering act

The steering message IS the agent-visible signal — it's the user-role
`AgentMessage` written in step 4/5. No `<steering-notification>` synthetic
element is required. The parent has no agent-visible record of having
steered (no parent-side `AgentToolCall` row by design); the audit trail is
the child session's transcript plus the queue metadata
(`metadata.queue.source = "steering"`).

## Concurrency And Hook Integration

### No parent-side tool-call rows

All five R4c tools are hook-intercepted **before** ordinary tool-call
persistence. None of them creates a parent-side `AgentToolCall` row.

This carries forward R4's pattern for `wait_subagent` and `cancel_subagent`:
management tools that operate on existing rows must not pollute the parent's
own transcript with management-tool noise, and must not invent extra bridge
edges. The Lean single-live-foreground invariant from R4 is preserved.

### Snapshot semantics

All five tools are point-in-time reads or one-shot writes. None blocks. None
long-polls.

- `list_subagents` / `list_background_tools`: single DB query. The response
  carries `read_at: ISO8601`. A row that terminalizes between query and
  response shows as its earlier status; the next call sees the update.
- `read_subagent_transcript`: single DB query against `AgentMessage`.
  Concurrent compaction is safe because the `next_sequence` cursor is stable
  under sequence-preserving compaction (#184).
- `read_tool_output`: ring buffer read for running rows (locked snapshot);
  immutable persisted payload for terminal rows.
- `steer_subagent`: one-shot write composing atomic existing transitions.

### File ownership

- `crates/defra-agent/src/background_tools.rs` (post-R6 rename): adds the
  five new tool definitions and their hook interceptors. ~300-500 new LoC
  over R4+R6 baseline.
- `crates/defra-agent/src/background_completion.rs` (post-R6 rename):
  unchanged. R4c does not extend the projector.
- New file: `crates/defra-agent/src/background_tools/transcript_render.rs`
  — pure-function transcript renderer (compact text from
  `Transcript.MessageRow` list). Unit-testable in isolation.
- `crates/defra-agent/src/hook.rs` and
  `crates/defra-agent/src/hook/persistence.rs`: hook interceptor entries for
  the five new tools.
- `crates/defra-agent/src/tool_surface/selection.rs`: registration gate
  wiring. `subagent_steering_enabled` flag governs
  `read_subagent_transcript` and `steer_subagent`. `subagent_targets`
  non-empty governs `list_subagents`. `backgroundable_tool_names` non-empty
  governs `list_background_tools` and `read_tool_output`.

## Verified Obligations

R4c expects **zero new Lean modules** and **zero new Lean transitions**.
R4c ships **one new theorem**.

### New theorem (required)

`steer_subagent_interrupt_preserves_link_symmetry`: after the
`interrupt = true` composition (interrupt + cascade + drain + append),
every bridge row's `parent_request_id` still points at a request whose
`caused_by_parent_*` symmetry is intact (B5 invariant preserved).

Statement (informal):

```lean
theorem steer_with_interrupt_preserves_link_symmetry
    (pre post : BackgroundedState)
    (steer : SteerWithInterrupt pre post)
    (h_pre : B5 pre) :
    B5 post
```

Discharged mechanically from existing B5 plus the existing `interrupt`
transition's invariants. The composition (interrupt + cascade + drain +
append) is the operationally-touchiest R4c path; the theorem makes future
regressions of the composition trip Lean before they trip Rust. Cost is one
short proof; gain is that any future steering refactor that breaks lineage
symmetry fails `lake build`. R4c is the cheapest place to add it.

### Conformance witnesses (required)

R4c emits Lean conformance witnesses for these scenarios so Rust tests
detect drift:

1. **`list_subagents` lineage scoping rejects a `child_request_id` from a
   sibling parent request.** Two siblings of the same parent request P1 and
   P2; P1's `list_subagents` does not return P2's children.
2. **`read_subagent_transcript` `next_sequence` cursor advances correctly
   across a paged call.** Two consecutive calls with the second's
   `since_sequence` = first's `next_sequence` cover the full transcript
   without overlap or gap.
3. **`read_subagent_transcript` rejects bridge call rows from the rendered
   output.** A child session containing a backgrounded tool call has the
   bridge row hidden in the rendered transcript.
4. **`read_tool_output` for a terminal row reads the persisted
   `<tool-completion>` payload, not a stale buffer.** After bridge
   projection, the in-memory buffer is gone; the read returns the persisted
   payload.
5. **`steer_subagent(interrupt=false)` appends with
   `caused_by_parent_request_id = caller`.** The new child-session
   `AgentRequest` preserves caller lineage.
6. **`steer_subagent(interrupt=true)` interrupts the active child request,
   drains automated wake-ups under the right queue key, and appends the
   steering message.** All three effects observable in one composition.

Witnesses register in `Proofs/Conformance/Contracts.lean` and Rust
consumers parse them in `state_machine_conformance.rs`. No new Lean module
is required.

### Lean properties to re-verify

After R4c, the following must remain green:

- All R4 request-lifecycle theorems S1, S3, S4, S5, S6.
- All R6-parametric bridge theorems B1, B2, B3, B4, B5, B6, B7.
- The R4a session-queue invariants (created-ordered pending list, unique
  coalesced queue keys).
- L1 bounded termination, L3 recovery convergence.
- #189 recovery enumeration coverage (unchanged by R4c).
- #191 transcript pair atomicity (unchanged by R4c).

R4c does not change any of these; it reuses them.

## Out Of Scope

Explicit non-goals; the implementation plan should not absorb them:

- A unified `list_background_work` / `read_background_transcript`.
  Explicitly chosen against in Section "Tool Surface".
- A `read_tool_output` `since_byte` incremental protocol. Snapshot per call.
- Foreground-steering surface (no tool to steer a foreground subagent; the
  parent is blocked anyway).
- Cross-deployment listing / reading / steering. v1 is local-only; the
  envelopes carry `deployment_id` so v2 lift is additive. R5's domain
  (see "Cross-Deployment v2 Coordination" below).
- A separate `replace_subagent` verb. We picked the boolean knob
  (`interrupt = false | true`).
- A new Lean transition for steering. Reuse-only.
- Per-DID / per-behavior list scopes. R4c is per-parent-request only.
- Operator/supervisor UI tools that show every behavior's live work. Future
  surface, not LLM-facing.
- Mid-execution tool steering (e.g., "send a SIGUSR1 to the running bash
  and let it keep running"). There is no design for this; if it ever lands,
  it's a separate tool.
- Live transcript tail / long-poll variants. Snapshot posture is canonical.

## Cross-Deployment v2 Coordination

R4c v1 is local-only: `list_subagents` and `list_background_tools` enumerate
work whose lineage lives on this deployment. The envelopes carry
`deployment_id` (set to the local deployment in v1) so a v2 lift to
cross-deployment is additive — the field is reserved, not invented later.

The v2 coordination point is not decided in R4c. R5's current spec covers
cross-deployment subagent **cancel propagation** and **completion
projection**, but does not address cross-deployment **listing**. Two
plausible v2 paths, both deferred:

1. **R4c v2 extends in place.** `list_subagents` and `list_background_tools`
   follow lineage across deployments via whatever cross-deployment query
   primitive R5 establishes. The envelopes don't change; the
   `deployment_id` field starts carrying remote values. Requires R5 to
   surface a "list children visible to this parent across deployments"
   query that R4c can call.
2. **A separate cross-deployment list tool supersedes R4c's.** R5 (or a
   later iteration) ships `list_remote_subagents` / `list_remote_background_tools`
   as new tools alongside R4c's, with their own envelopes. R4c's tools
   remain strictly local. Larger surface; clearer coordinate-system split.

R4c does not pick. The `deployment_id` field is the seam that lets either
path land additively. When R5 has consensus on the cross-deployment query
surface, the v2 decision becomes mechanical.

## Approval Checklist

Before implementation planning, Jack should approve:

- The five-tool surface, split per kind: `list_subagents`,
  `list_background_tools`, `read_subagent_transcript`, `read_tool_output`,
  `steer_subagent`.
- Registration gates: `subagent_targets` non-empty for the subagent family,
  `subagent_steering_enabled` for `read_subagent_transcript` and
  `steer_subagent`, `subagent_background_enabled` additionally for
  `steer_subagent`, `backgroundable_tool_names` non-empty for the tool
  family.
- Per-parent-request lineage scope for the two list tools.
- `read_subagent_transcript` defaults: compact text, assistant-only,
  `since_sequence` paged, hard caps 100 messages / 24000 chars / 256-byte
  tool-result snippets; bridge call rows always hidden.
- `read_tool_output` defaults: snapshot per call, tail of ring buffer for
  running rows, persisted `<tool-completion>` payload for terminal rows;
  per-stream cap 16 KB default / 256 KB hard cap; best-effort UTF-8 trim;
  `total_bytes_seen` monotonic counter.
- `steer_subagent` ships both modes in v1: `interrupt = false` (append)
  and `interrupt = true` (interrupt + cascade + drain + append). Composes
  existing primitives — zero new Lean transitions.
- No parent-side `AgentToolCall` rows for any of the five tools.
- Zero new Lean modules. One new theorem
  (`steer_subagent_interrupt_preserves_link_symmetry`, B5 preservation
  under the interrupt + cascade + drain + append composition). Six
  conformance witnesses cover observable Rust shapes.
- Cross-deployment deferred to v2 (R5's domain). `deployment_id` on
  envelopes for additive v2 lift. R4c does not pick between v2 extending
  in place vs. a separate cross-deployment list tool; the `deployment_id`
  seam supports either.
- Sequencing: R4c executes after R6 merges to main; PR rebases on R6 merge;
  no "pure rename" no-op commit in R4c (R6 owns that).
