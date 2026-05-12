# R4 Agent-Facing Subagent Tools Design

Status: draft for Jack approval, revision 2
Date: 2026-05-12
Depends on:

- `docs/superpowers/specs/2026-05-08-subagent-lifecycle-design.md`
- `docs/superpowers/specs/2026-05-08-r2-rust-subagent-data-plane-design.md`
- `docs/superpowers/specs/2026-05-11-r3-rust-subagent-source-design.md`
- `docs/superpowers/specs/2026-05-08-tool-call-runtime-closure.md`
- deadline preservation through request claim, folded into R4a

## Goal

R4 ships in two ordered parts:

1. **R4a:** Lean queue model plus Rust scheduler change. Same-session
   `AgentRequest` rows become a real FIFO queue instead of being treated as
   duplicates to supersede.
2. **R4b:** the v1 agent-facing subagent tools that depend on that queue:
   `spawn_subagent`, `wait_subagent`, and `cancel_subagent`.

The follow-up R4c tool surface (`list_subagents`,
`read_subagent_transcript`, and `steer_subagent`) is designed here so R4b does
not paint it into a corner, but it is not part of the first implementation
plan.

R4 exposes first-class subagent management to an LLM running inside
defra-agent. The v1 tools let a parent agent spawn a child behavior, observe
its terminal result, and stop it. R4a also makes the request/session model
explicit enough to support background subagent completion notifications and
same-session follow-up work.

The Lean proofs remain the source of truth. R4 should not invent new
`AgentToolCall` transitions. Subagent tools use the R2 transitions already
landed: `background`, `foreground`, `bridge_complete`, `bridge_failure`, and
`bridge_cancel_cascade`. The request-queue behavior described below changes
`AgentRequest` scheduling semantics and must start in Lean before Rust
implementation, per `CLAUDE.md`.

R4a touches the current proof files that own request lifecycle and
session/request selection semantics:

- `crates/defra-agent/proofs/Proofs/Request/State.lean`
- `crates/defra-agent/proofs/Proofs/Request/Transition.lean`
- `crates/defra-agent/proofs/Proofs/Request/Executable.lean`
- `crates/defra-agent/proofs/Proofs/Request/Properties.lean`
- `crates/defra-agent/proofs/Proofs/Composed.lean`
- `crates/defra-agent/proofs/Proofs/RuntimeReconcile/State.lean`
- `crates/defra-agent/proofs/Proofs/RuntimeReconcile/Transition.lean`
- `crates/defra-agent/proofs/Proofs/RuntimeReconcile/Executable.lean`
- `crates/defra-agent/proofs/Proofs/SessionRecovery.lean`

There is no `Proofs/Session/` directory today. If the R4a queue model grows
beyond the existing `SessionRecovery.lean` abstraction, create
`Proofs/Session/State.lean`, `Transition.lean`, `Executable.lean`, and
`Properties.lean`, then import them from `Proofs.lean`.

## External References Studied

Codex exposes a subagent tool family around `spawn_agent`, `wait_agent`,
`send_input`, `close_agent`, and `list_agents`. Its background completion
watcher injects a user-role notification into the parent thread without
starting a turn; explicit `send_input(interrupt=true)` is the path that
interrupts and replaces work.

The two Pi subagent examples use the same broad shape: foreground calls block
and return inline, background calls return a handle and later notify the
originating session, and steering/resume is a follow-up operation on the child.

R4 adopts those ergonomics, but maps them onto defra-agent's durable
`AgentRequest`, `AgentMessage`, and `AgentToolCall` rows.

## Tool Surface

R4b v1 keeps the agent-facing surface intentionally small. The expected
first-90-days prompts need three things: delegate work, wait for delegated
work, and stop delegated work. They do not need discovery (`list_subagents`)
because spawn returns the handle, and they do not need transcript/steering
until we have real prompts that ask the parent to supervise long-running child
conversations. Deferring those tools avoids shipping queue semantics, bridge
projection, transcript rendering, and steering all in one implementation wave.

R4b registers these tools when the parent behavior's
`ToolSelectionDocument.subagent_spawn_enabled = true` and the behavior has at
least one `subagent_targets` entry:

1. `spawn_subagent`
2. `wait_subagent`
3. `cancel_subagent`

R4c follow-up tools:

4. `list_subagents`
5. `read_subagent_transcript`
6. `steer_subagent`

`read_subagent_transcript` and `steer_subagent` remain gated by
`subagent_steering_enabled` when R4c lands. `list_subagents` can be gated by
`subagent_background_enabled` or promoted to the core spawn surface once there
is a concrete caller.

`spawn_subagent` accepts `await_mode: "foreground" | "background"` instead of
splitting spawn into two tools. The default is `"foreground"`.
`await_mode="background"` additionally requires
`ToolSelectionDocument.subagent_background_enabled = true`.

There is no agent-facing `background_subagent` tool. If a foreground wait needs
to be backgrounded, that is a user/operator action. Timer-based
auto-backgrounding is out of R4 until there is a concrete user or prompt that
needs it.

## Authorization

Before creating a child request, the tool must verify that `behavior_id` is in
the parent behavior's `ToolSelectionDocument.subagent_targets`.

Unauthorized targets are rejected before child materialization. The LLM-facing
result uses the existing structured tool error convention:

```json
{
  "ok": false,
  "failure_class": "tool_not_allowed",
  "path": "/behavior_id",
  "message": "behavior 'x' is not allowed as a subagent target for this behavior",
  "retryable": false,
  "service_id": "subagent",
  "tool_name": "spawn_subagent",
  "requested_tool_name": "x",
  "allowed_subagent_targets": ["..."]
}
```

The persisted lifecycle failure class is
`FailureClass::ServiceUnavailable`, preserving the existing five-class trace
contract while keeping raw `tool_not_allowed` visible to trace exporters.

Implementation note: R3 `SubagentSource` currently validates that the target
behavior exists in the active runtime snapshot, but not that it is authorized
by the parent's `subagent_targets`. R4 chooses defense in depth: the
agent-facing spawn path, `SubagentSource`, and orphan subagent recovery all
must resolve the parent request's behavior/tool selection and reject an
unauthorized target before child materialization. Rejection uses the same
LLM/trace shape as above (`ServiceUnavailable` persisted class,
`tool_not_allowed` raw structured error). This grows R4 into R3-owned code, but
it prevents any future bridge-row writer from bypassing the agent-facing guard.

## Depth Ceiling

R4 follows Lean/R2 exactly:

- `MAX_SUBAGENT_DEPTH = 3`
- a parent at depth `0`, `1`, or `2` may spawn
- a parent at depth `3` is rejected

The check is `parent.subagent_depth + 1 > MAX_SUBAGENT_DEPTH`, not the older
R4 prompt wording of `subagent_depth >= 4`.

Depth rejection returns a structured LLM-facing error with code
`subagent_depth_exceeded` and persists an `ArgumentInvalid` tool failure.

## Deadline Preservation

R4 folds the subagent deadline preservation fix into scope. The
`spawn_subagent.deadline` argument is otherwise misleading: today
`RequestLifecycle::claim_inner` overwrites `AgentRequest.deadline` with
`now + behavior.deadline_duration`, so a materialized child deadline can be
dropped at claim time.

R4a updates the Lean request claim model and Rust claim path so an existing
request deadline survives claim. If no deadline is present, claim continues to
set the behavior-duration deadline. If a deadline is present, claim preserves
that earlier explicit deadline and never extends it beyond the parent request
deadline.

Required regression coverage:

- materialize a subagent request with an explicit deadline earlier than the
  child's behavior duration
- claim the child request
- assert the persisted `AgentRequest.deadline` remains the explicit deadline
- assert foreground `spawn_subagent` still treats the parent request deadline
  as the upper bound for waiting

## Request Queue Semantics

R4 needs a per-session request queue. `AgentRequest` is already close to that
queue: rows are durable, scoped to a `session_id`, and ordered by `created_at`.
The missing semantic is that current same-session pending/processing rows are
treated as duplicates and later pending rows are superseded. Background
subagent notifications and steering need "queued behind active", not
"duplicate".

R4a introduces the queue semantics before the agent-facing tools rely on it:

- A session has at most one active runtime request.
- Additional same-session requests remain `pending` until earlier active work
  reaches a terminal state.
- Pending rows are selected in `created_at` order.
- Same-session pending rows are not superseded merely because an earlier
  request is active.
- Existing status/lifecycle terminal vocabulary is retained.

Because this changes request lifecycle behavior, the Lean request/session
model must be updated before the Rust scheduler changes.

### Queue Metadata

R4 uses `AgentRequest.metadata` for queue hints rather than adding schema
fields:

```json
{
  "queue": {
    "source": "subagent_completion",
    "policy": "coalesce",
    "key": "subagent_completion:<parent_session_id>",
    "queued_after_request_id": "active-parent-request-id"
  }
}
```

Initial policies:

- `append`: create a distinct pending request.
- `coalesce`: if a pending same-session request with the same queue key exists,
  keep one wake-up request and rely on durable transcript notifications for the
  details.

Automated subagent completion wake-ups use:

- `execution_origin = "scheduled"`
- `metadata.queue.source = "subagent_completion"`
- `metadata.queue.policy = "coalesce"`

Explicit user input and steering use append semantics unless an interrupt
replacement path says otherwise.

### Queue Cancellation

User cancellation has priority over automated wake-ups:

- Canceling an active request interrupts that request.
- Live child tool/subagent edges cascade through the existing
  `bridge_cancel_cascade` transition.
- Pending automated wake-up requests for the canceled session are terminalized
  as interrupted/superseded; rows are not deleted.
- If the user provides replacement input, that replacement is the next queued
  user-originated request and is not blocked by stale automated wake-ups.
- Durable `AgentMessage` notifications remain in history as audit/context.

Session-level cancellation drains active work plus pending queue entries for
the session.

## Child Sessions

Every spawned subagent gets a fresh child session. R2 already creates a fresh
`session_id` in `create_subagent_request_with_request_id`.

The parent-child linkage is:

- parent `AgentRequest.request_id`
- parent `AgentToolCall.tool_call_id`
- child `AgentRequest.caused_by_parent_request_id`
- child `AgentRequest.caused_by_parent_tool_call_id`
- parent `AgentToolCall.child_request_id`

Spawn results return both `child_request_id` and `child_session_id`. Follow-up
tools use `child_request_id` as the primary handle and resolve the child
session through the authorized parent-child edge. `child_session_id` is useful
context, not sufficient authority by itself.

## `spawn_subagent`

Arguments:

```json
{
  "behavior_id": "target-behavior",
  "prompt": "work to delegate",
  "await_mode": "foreground",
  "deadline": "optional RFC3339 deadline"
}
```

Rules:

- `await_mode` defaults to `"foreground"`.
- `cancel_policy` is not exposed in R4. It is hard-coded to `"cascade"`.
- `behavior_id` must be authorized by `subagent_targets`.
- `deadline`, when provided, must be at or before the parent request deadline.
- Child creation must pass the Lean/R2 depth guard.
- Background spawn returns after the child `AgentRequest` is materialized, not
  after the child reaches terminal.
- Foreground spawn blocks until the child reaches terminal, the parent deadline
  wins, parent cancellation wins, or the wait is backgrounded by a user/operator
  action.

Background return shape:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "await_mode": "background",
  "status": "running"
}
```

Foreground success return shape:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "await_mode": "foreground",
  "status": "completed",
  "final_response": "...",
  "error": null
}
```

Foreground terminal failure return shape:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "await_mode": "foreground",
  "status": "failed|dead|interrupted|superseded",
  "final_response": null,
  "error": {
    "reason": "...",
    "failure_class": "serviceUnavailable|argumentInvalid|transport|toolReturnedError|external"
  }
}
```

`status` maps directly to R2 `ChildTerminal` vocabulary:

- `ChildTerminal::Failed` -> `failed`
- `ChildTerminal::Dead` -> `dead`
- `ChildTerminal::Interrupted` -> `interrupted`
- `ChildTerminal::Superseded` -> `superseded`

Only `failed` carries the child failure class. Other terminal states get a
synthetic reason that names the terminal condition.

`final_response` is read from the child session's materialized final assistant
`AgentMessage`, using `AgentResponse.materialized_message_sequence` as the
anchor when available. It must not read `AgentResponse.content`, because issue
#64 made that field a live tail that is cleared on finalize.

### Foreground Deadline And Cancellation

Foreground waiting must respect the parent request deadline. If the parent
deadline passes while the tool is waiting, R4 calls
`bridge_failure(ChildTerminal::Dead)` on the parent bridge row and returns a
synthetic failure envelope to the LLM.

If the parent request is canceled while a foreground or background child is
live, R4 uses the existing parent tool-call cancellation path:

1. parent tool call is canceled
2. `bridge_cancel_cascade()` returns `CascadeIntent::Cancel`
3. the child request is interrupted

R4 must not add a new cancel transition.

### User/Operator Backgrounding Of Foreground Waits

Foreground waits may be backgrounded by a user/operator action:
"background this subagent". That path calls the existing `background()`
transition on the parent
`AgentToolCall`. The child continues running. The blocked foreground tool
returns:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "await_mode": "background",
  "status": "running",
  "backgrounded": true
}
```

Timer-based auto-backgrounding is deliberately out of R4. If a real caller
needs it later, it should land as a small follow-up PR that adds the
configuration field and runtime policy together.

## Background Completion

When a background child reaches terminal, R4 projects the terminal state onto
the original parent bridge row:

- completed -> `bridge_complete(final_response)`
- failed/dead/interrupted/superseded -> `bridge_failure(child_terminal)`

R4 then appends a compact synthetic user-role notification into the parent
session transcript. It is durable model-visible context, not a duplicate tool
result for the original spawn call.

Notification shape:

```xml
<subagent-notification
  child_request_id="..."
  child_session_id="..."
  behavior_id="..."
  status="completed">
  <summary>compact result or terminal summary</summary>
</subagent-notification>
```

After appending the notification, R4 enqueues a same-session automated wake-up
request with `metadata.queue.source = "subagent_completion"` and
`execution_origin = "scheduled"`, subject to the coalescing and cancellation
rules above. The wake prompt should be small, for example:

```text
Process pending subagent completion notifications in this session.
```

The actual details live in the durable notification messages.

### Interleaving Cases

Worked example:

1. Parent request `P` is processing in parent session `S`.
2. `P` has foreground child `A`; the parent model is blocked inside
   `spawn_subagent(await_mode="foreground")`.
3. `P` also has a separate background child `B`.
4. `B` reaches terminal while `A` is still running.

The expected behavior is:

- `B` projects onto its original bridge row immediately:
  `bridge_complete(final_response)` for success, or
  `bridge_failure(child_terminal)` for failure/dead/interrupted/superseded.
- `B` appends a durable synthetic user-role
  `<subagent-notification>` message to parent session `S` immediately. Durable
  transcript writes are not gated on the parent model being blocked on
  foreground child `A`.
- R4 enqueues an automated wake-up `AgentRequest` in session `S` with:

```json
{
  "execution_origin": "scheduled",
  "metadata": {
    "queue": {
      "source": "subagent_completion",
      "policy": "coalesce",
      "key": "subagent_completion:S",
      "queued_after_request_id": "P"
    }
  }
}
```

- That wake-up remains `pending` while `P` is active. It is not claimed when
  `A` merely returns to the parent model; it waits until `P` itself reaches a
  terminal request state.
- When `P` terminalizes, the normal watcher/router pending-pickup path selects
  the next eligible request in session `S` by `created_at`. If the wake-up is
  next, the parent behavior claims it and processes the notification context
  already present in history.
- If background children `B`, `C`, and `D` all complete while `P` is active,
  they each append their own durable transcript notification, but their wake-up
  requests coalesce under the same queue key:
  `subagent_completion:<parent_session_id>`. The queue contains one pending
  automated wake-up for session `S`, not one wake-up per child.

If the user cancels `P` before it terminalizes, the cancellation rules above
drain the pending automated wake-up while keeping the already-appended
notifications in session history.

## `wait_subagent`

Arguments:

```json
{
  "child_request_id": "..."
}
```

`wait_subagent` authorizes through the parent-child edge and returns the same
terminal envelope as foreground `spawn_subagent`.

If the child is still running, the parent waits until the child is terminal or
the parent deadline/cancellation/backgrounding path wins. Waiting does not
create a new child session or a new parent-child edge.

`wait_subagent` does not get its own `AgentToolCall` row. The hook intercepts
the call, authorizes it, foregrounds/observes the existing bridge row, waits,
and returns the terminal envelope directly. This preserves the Lean
single-live-foreground invariant: there is one foreground bridge edge, not a
native `wait_subagent` tool row plus the original subagent bridge row.

## R4c Follow-Up: `list_subagents`

Arguments:

```json
{
  "status": "running|terminal|all",
  "limit": 20
}
```

Returns compact entries for children reachable from the current parent
request/session's authorized subagent edges:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "behavior_id": "...",
  "await_mode": "background",
  "status": "running",
  "created_at": "...",
  "last_update": "..."
}
```

The tool does not list arbitrary sessions or children not linked to the
current parent.

## R4c Follow-Up: `read_subagent_transcript`

Arguments:

```json
{
  "child_request_id": "...",
  "since_sequence": 0,
  "limit": 20,
  "max_chars": 6000,
  "include_tool_results": false
}
```

The return value is compact rendered text optimized for LLM consumption, not
raw `AgentMessage` rows:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "from_sequence": 1,
  "through_sequence": 12,
  "truncated": false,
  "transcript": "..."
}
```

By default, only user/assistant text is included. Tool-result bodies are
omitted unless `include_tool_results = true`, and even then remain bounded by
`limit` and `max_chars`.

## R4c Follow-Up: `steer_subagent`

Arguments:

```json
{
  "child_request_id": "...",
  "message": "...",
  "interrupt": false
}
```

`steer_subagent` is valid only for background subagents. A foreground child
blocks the parent model, so the parent cannot decide to steer it.

Semantics:

- authorize through the parent-child edge
- append a user-role steering message to the child session
- enqueue same-session child work preserving child behavior and subagent depth
- do not create a new child session
- do not create a new parent-child edge

`interrupt` defaults to `false`.

- `interrupt: false`: queue the steering work behind active child work.
- `interrupt: true`: interrupt the child session's active request, cascade to
  the child's live tools/subagents, drain automated wake-ups in the child
  session, and enqueue the steering message as replacement work.

This depends on the R4a request queue semantics. If existing `AgentRequest`
coherence or B5 link symmetry does not permit multiple same-session subagent
requests to carry the original parent provenance, update the Lean/data-plane
model before implementing this tool.

## `cancel_subagent`

Arguments:

```json
{
  "child_request_id": "...",
  "reason": "optional human-readable reason"
}
```

`cancel_subagent` means "stop that subagent", not just "interrupt its current
turn".

Semantics:

- authorize through the parent-child edge
- interrupt the child session's active request, if any
- cascade-cancel live descendants through existing bridge cancellation
- drain queued child-session requests created by steering or automated wake-ups
- leave child transcript and terminal history intact
- return a compact status envelope with interrupted/drained counts

## Hook And Tool Runtime Integration

R4b tools must be registered as native Rig tools only when enabled by the
parent behavior's `ToolSelectionDocument`. R4c tools follow the same rule when
they land.

Spawn and bridge projection cannot use the existing native tool completion
path unchanged. Existing hook persistence calls `ToolCallLifecycle::new` for
ordinary tools and later calls native `complete`/`fail`. R2 explicitly rejects
native `complete`/`fail` on subagent-typed tool calls. Therefore R4 must add a
subagent-aware hook/runtime path that:

- parses and validates subagent tool args early
- creates subagent bridge rows with `ToolCallLifecycle::new_subagent`
- persists `await_mode`, `cancel_policy = cascade`, and `child_request_id`
- never native-completes a subagent bridge row
- uses `bridge_complete`/`bridge_failure` for child terminal projection
- uses `bridge_cancel_cascade` for parent cancellation
- intercepts `wait_subagent` before ordinary Rig tool-call persistence and
  returns from the existing bridge row without writing a separate
  `AgentToolCall`

Management tools that operate on an existing child edge (`wait_subagent`,
`steer_subagent`, `cancel_subagent`, `read_subagent_transcript`,
`list_subagents`) must not create extra child edges.

The implementation may use hook interception, request-scoped tool runtime
context, or a combination of both. The required observable behavior is the
state-machine behavior above.

## Out Of Scope

- Cross-deployment subagents.
- Detachable children. R4 hard-codes cascade.
- Multi-parent/fork semantics.
- Token/cost budget propagation.
- Streaming child output into the parent before terminal notification.
- User-facing UI for backgrounding foreground waits, beyond the runtime hook
  points and state transitions needed by R4.

## Approval Checklist

Before implementation planning, Jack should approve:

- R4 shipping as R4a queue semantics followed by R4b tools
- the R4b v1 surface: `spawn_subagent`, `wait_subagent`, `cancel_subagent`
- R4c deferral for `list_subagents`, `read_subagent_transcript`, and
  `steer_subagent`
- single `spawn_subagent` with `await_mode`
- request queue semantics as an R4a prerequisite
- metadata-based queue hints
- deadline preservation through claim as R4 scope
- SubagentSource/recovery authorization hardening as R4 scope
- background completion notification plus scheduled coalesced wake-up
- cancellation/drain behavior
- steering as child-session queue work
- `cancel_subagent` canceling the child session queue
- foreground backgrounding as user/operator path, not agent-facing tool
- no timer auto-backgrounding in R4
