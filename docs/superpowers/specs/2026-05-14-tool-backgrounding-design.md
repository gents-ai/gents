# R6 Agent-Facing Tool Backgrounding Design

Status: draft for Jack approval, revision 1
Date: 2026-05-14
Tracking: TBD (suggested title: "R6: agent-facing tool backgrounding")
Refs: #183, #177, #191, #189, #190, #184, #160

Depends on:

- `docs/superpowers/specs/2026-05-12-r4-agent-facing-tools-design.md`
- `docs/superpowers/specs/2026-05-14-agent-response-streaming-lean-design.md` (#190; cited for vocabulary only — implementation sequenced after #190 merges)
- compaction tool-call-pair preservation (#184; cited so the implementation plan can sequence after it lands)

## Goal

R6 generalizes R4's `await_mode = background` from subagent bridge rows to
ordinary `AgentToolCall` rows. The agent-facing surface gains three new tools:

1. `background_tool(tool_name, args)` — meta-tool that proxies any
   backgroundable tool with `await_mode = background`, returning a
   `tool_call_id` handle immediately.
2. `wait_tool(tool_call_id)` — hook-intercepted; foregrounds the existing
   backgrounded row and returns the terminal envelope.
3. `cancel_tool(tool_call_id)` — hook-intercepted; cancels the backgrounded row
   through the existing `bridge_cancel_cascade` (now parametric).

The normal foreground tool call path (`bash(cmd)`, `read_file(...)`) is
unchanged. There is no `call_tool` meta-tool; foreground invocation is the
existing tool surface.

R6's design discipline is **reuse-first**. The strategic claim of this work is
that the bridge pattern proven in R4 already generalizes; almost everything in
this spec is a parametrization of existing modules rather than a new module.
The §"Verified Obligations" table makes that claim concrete: a "Reuse target"
column names the existing module each obligation lands in, and the
new-module/new-theorem rows fit on one hand.

## Why Now

The formal-coverage audit (`docs/superpowers/audits/2026-05-13-formal-coverage-audit.md`)
ranks the runtime at ~80% formally specified after #190 (streaming response
state machine) and #184 (compaction tool-call-pair preservation) land. R6 is
the first product feature designed against the (mostly) fully-modeled
runtime. The vocabulary it needs — `AwaitMode`, `CancelPolicy`, `ChildTerminal`,
the bridge transitions, the same-session queue, the transcript pair atomicity,
the recovery sweep enumeration — already exists in Lean. R6 reuses it.

The implementation plan should sequence execution after #190 and #184 merge
so streaming composition and compaction policy are referenced at canonical
vocabulary. The design file itself can land before #190/#184 merge.

## Verified Obligations

Every obligation lands in an existing module. The "Reuse target" column names
the module; "Generalization" names what specifically changes. Zero new
modules; one new theorem (B7).

| Obligation | Reuse target | Generalization |
|---|---|---|
| Backgrounded row reaches exactly one terminal | `Proofs/Background/Properties.lean` (renamed from `Proofs/Subagent/Properties.lean`) — B1 | Lift `child.request.state` reads in the antecedent to `terminalOf(second_leg)` via a kind-dispatched predicate. Theorem text essentially unchanged. |
| Bridge row never native-completed | B2 | Same parametrization. The "bridge row" identification reuses `callId = bridgeCallId`; the prohibition on `complete_native`/`fail_native` on bridge rows is kind-independent. |
| Cascade preserves child terminal once parent terminal | B3 | Generalize the cascade post-state via kind dispatch: Subagent → `interruptRequestedAt`; Tool → executor cancel-signaled. |
| Depth ≤ `maxSubagentDepth` (Subagent kind only) | B4 (kind-guarded) | Tool kind is depth-free; B4 becomes `∀ row, kind row = Subagent → depth ≤ maxSubagentDepth`. Existing proof carries. |
| Bridge link symmetry | B5 | Link field generalizes from `child_request_id : Option RequestId` to `link : BackgroundLink` (still optional `RequestId` for Subagent kind; trivial for Tool kind). |
| Unique call IDs | B6 | No change. Theorem already speaks only about `parent.tools`. |
| Per-parent budget ≤ 8 | `Proofs/Background/Properties.lean` (new theorem B7) | New theorem: for any reachable state, `count (parent.tools.filter (λ t → t.awaitMode = .background ∧ ¬terminal t.state)) ≤ 8`. Single new theorem; precondition is a guard in `bridge_spawn` (parametric). |
| Recovery sweep includes backgrounded rows | `Proofs/Recovery/` (#189 enumeration) | One new `RecoveryAction` variant `TerminalizeBackgroundedAsInterrupted`; one new clause in the existing match. Predicate: `awaitMode = .background ∧ state = .running ∧ ¬terminal parent.request.state`. |
| Tool-completion transcript pair atomicity | `Proofs/Transcript/` (#191) | Reuse existing pair atomicity verbatim. New transcript element name `<tool-completion>` shares dedupe path with #160 (`(session, logical_result_id, payload_hash)`). |
| Coalesced wake-up by `(session_id, queue_key)` | `Proofs/Session/` (R4a queue) | Reuse the queue model. Queue source string `subagent_completion` renames to `background_completion` (one-release alias for back-compat). Queue key for tool completion: `background_completion:<parent_session_id>`. |
| Streaming compose with `StreamingResponse.Status` (#190) | `Proofs/StreamingResponse/` | Not required for v1 (in-memory buffer only; no live streaming). Vocabulary cited so v2 streaming can reference it. |

Total new modules: **0**. Total new theorems: **1** (B7 budget bound).

## Architecture

### Module Rename

R6 renames `Proofs/Subagent/` → `Proofs/Background/` in Lean and mirrors in
Rust:

- `crates/defra-agent/src/subagent_completion.rs` → `background_completion.rs`
- `crates/defra-agent/src/subagent_tools.rs` → `background_tools.rs`

This is a breaking import change across the verified module set. It lands in
the same PR as the R6 implementation; the renamed file diffs are mechanical
and reviewable separately within the PR. The implementation plan calls the
rename out as its own task before the parametrization tasks so reviewers can
verify the rename is a no-op before reading the substantive changes.

The R4 spec, R3 spec, and R2 spec continue to refer to the modules by their
historical names. Reviewers reading those specs against the renamed source
tree should treat `Proofs/Subagent/X` and `Proofs/Background/X` as the same
module across the rename.

R4's semantics are preserved by construction. The rename is a parametric
generalization that admits Subagent as one of two kinds; every R4 transition
on Subagent-kind rows behaves identically pre and post rename. R6 does not
change R4 behavior; it generalizes the substrate R4 already lives on. The
R4 conformance witnesses and tests should pass against the renamed module
set without modification.

### Parametric BridgedState

`BridgedState` becomes parametric over a discriminator:

```lean
inductive BackgroundedKind where
  | Subagent   -- second leg is a child ComposedState
  | Tool       -- second leg is an in-process tool execution
```

`BridgedState`'s `child : ComposedState` field generalizes to a sum type
indexed by kind. For Subagent kind, the second leg is a `ComposedState` (as
today). For Tool kind, the second leg is the same `ToolCallContext` that the
bridge row IS — i.e., the bridge row's own state is the terminal source. The
"two-leg" Tool case is degenerate in the sense that the leg and the row are
the same row; the parametric form preserves the bridge transition semantics
without inventing a new transition.

The four R4 bridge transitions become parametric:

- `bridge_spawn`: kind-dispatched. Subagent kind allocates a child
  `ComposedState`; Tool kind allocates an `AgentToolCall` row with
  `awaitMode = background` and no child request. Both check the per-parent
  budget guard (B7).
- `bridge_complete`: fires when `terminalOf(second_leg) = Completed`. For
  Subagent: child request reached `.completed`; for Tool: tool's own
  `state = .completed`.
- `bridge_failure`: fires when terminal is non-completed
  (`Failed | Dead | Interrupted | Superseded`). Same dispatch.
- `bridge_cancel_cascade`: parent terminal + `cancel_policy = cascade`. For
  Subagent: set `child.interruptRequestedAt`; for Tool: signal in-process
  executor to cancel (kill bash subprocess, abort MCP call). Single
  transition; kind-dispatched effect in Rust.

### Tool Capability and Operator Allowlist

Each `Tool` impl declares `backgroundable: bool`. Bash and MCP tools declare
`true`; fast tools (`read_file`, `glob`, `grep`) declare `false`. The
declaration is a static property of the tool registration.

`ToolSelectionDocument` gains a new field
`backgroundable_tool_names: [String]`, mirroring `subagent_targets`. The
operator allowlist is a per-behavior gate.

Tool registration registers a tool as backgroundable for a behavior iff:

```
tool.backgroundable = true
∧ tool.name ∈ behavior.tool_selection.backgroundable_tool_names
```

The agent-facing `background_tool` meta-tool rejects unknown / non-backgroundable
target names with a structured error before allocating a bridge row:

```json
{
  "ok": false,
  "failure_class": "tool_not_allowed",
  "path": "/tool_name",
  "message": "tool '<name>' is not allowed for backgrounding by this behavior",
  "retryable": false,
  "service_id": "background",
  "tool_name": "background_tool",
  "requested_tool_name": "<name>",
  "allowed_backgroundable_tool_names": ["..."]
}
```

The persisted lifecycle failure class is `FailureClass::ServiceUnavailable`,
matching R4's authorization-failure shape.

## Authorization

`background_tool` authorization is checked in three places (defense in depth,
matching R4's R3-hardening pattern):

1. **Agent-facing call site.** The hook intercepting `background_tool`
   resolves the parent behavior's `ToolSelectionDocument`, looks up the
   target tool's capability bit, checks the allowlist, and rejects before
   any `AgentToolCall` row is written.
2. **Bridge spawn Rust-side guard.** The parametric `bridge_spawn` for the
   Tool kind has the same Lean preconditions as Subagent kind (parent
   processing, B7 budget), plus a Rust-side eligibility guard mirroring
   R4's `subagent_targets` enforcement. The Lean model does not carry the
   allowlist (the existing R4 model treats `subagent_targets` the same
   way); the allowlist guard is the Rust-side adapter that decides whether
   to call `bridge_spawn` at all.
3. **Recovery-time validation.** On `recover_all`, when a backgrounded row
   is encountered, the sweep re-resolves the parent behavior's allowlist.
   If the target tool is no longer authorized (e.g., operator removed it
   from the allowlist between runs), the row is terminalized as
   `.interrupted` with reason `tool_not_allowed_at_recovery`. This prevents
   an authorization downgrade from leaving a row running.

`wait_tool` and `cancel_tool` authorize through the existing bridge row
edge: the caller must be the parent request that owns the `tool_call_id`.
No allowlist re-check at wait/cancel time; the row's existence is the
authorization witness.

## Budget Ceiling

Per-parent-request ceiling on concurrent backgrounded tools:

- `MAX_BACKGROUNDED_TOOLS_PER_PARENT = 8`
- count `= |{ t ∈ parent.tools | t.awaitMode = .background ∧ ¬terminal t.state }|`
- a `background_tool` call with `count + 1 > MAX_BACKGROUNDED_TOOLS_PER_PARENT`
  is rejected

Rejection returns a structured LLM-facing error with code
`background_tool_budget_exceeded` and persists an `ArgumentInvalid` tool
failure on the parent (no bridge row is written for the rejected call). The
error includes the current count and the cap so the agent can decide whether
to `wait_tool` on an existing handle.

```json
{
  "ok": false,
  "failure_class": "argument_invalid",
  "path": "/",
  "message": "parent request has reached the concurrent backgrounded tool ceiling (8)",
  "retryable": false,
  "service_id": "background",
  "tool_name": "background_tool",
  "current_backgrounded": 8,
  "max_backgrounded": 8
}
```

There is no per-session ceiling and no per-tool-name budget in v1. If either
becomes load-bearing, it should land as a small follow-up PR with a real
caller behind it.

The budget is enforced by Lean theorem B7 (count ≤ 8 invariant under any
reachable trace), guarded by the `bridge_spawn` precondition for Tool kind.

## Tool Detail

### `background_tool`

Arguments:

```json
{
  "tool_name": "bash",
  "args": {
    "cmd": "long-running-command"
  }
}
```

Rules:

- `tool_name` must satisfy the eligibility predicate (capability bit ∧
  allowlist).
- The parent request's concurrent backgrounded-tool count must be ≤ 7.
- The proxied tool's argument validation must pass before the bridge row is
  allocated.
- A fresh `AgentToolCall` row is allocated with `awaitMode = background`,
  `cancelPolicy = cascade`, no `child_request_id`. The tool's executor is
  launched in-process; stdout/stderr are captured into an in-memory ring
  buffer keyed by `tool_call_id`.
- The call returns immediately with the handle, not after the tool reaches
  terminal.

Return shape (success path):

```json
{
  "tool_call_id": "...",
  "tool_name": "bash",
  "await_mode": "background",
  "status": "running"
}
```

If the eligibility, budget, or argument-validation checks reject the call,
the standard structured error envelope returns instead.

### `wait_tool`

Arguments:

```json
{
  "tool_call_id": "..."
}
```

`wait_tool` is hook-intercepted before ordinary Rig tool-call persistence.
It does **not** allocate a second `AgentToolCall` row — it foregrounds and
observes the existing backgrounded row, preserving the Lean
single-live-foreground invariant.

Semantics:

- authorize through the existing parent → tool-call edge (`tool_call_id`
  must be a backgrounded row owned by the calling parent request)
- if the row is already terminal, return its captured envelope immediately
- otherwise block until the row reaches terminal, the parent request
  deadline wins, the parent request is cancelled, or a user/operator
  backgrounding action re-detaches the wait

Return shape (terminal):

```json
{
  "tool_call_id": "...",
  "tool_name": "bash",
  "await_mode": "background",
  "status": "completed",
  "result": {
    "stdout": "...",
    "stderr": "...",
    "exit_code": 0
  },
  "error": null
}
```

Return shape (failure / interrupted / cancelled):

```json
{
  "tool_call_id": "...",
  "tool_name": "bash",
  "await_mode": "background",
  "status": "failed",
  "result": {
    "stdout": "<captured-so-far>",
    "stderr": "<captured-so-far>",
    "exit_code": null
  },
  "error": {
    "reason": "...",
    "failure_class": "argumentInvalid|transport|toolReturnedError|external|serviceUnavailable"
  }
}
```

`result` is the native terminal payload of the proxied tool. The envelope
shape is uniform across tools; the `result` body is tool-specific (bash:
`{stdout, stderr, exit_code}`; MCP: result body).

If the parent request deadline passes during a wait, `wait_tool` returns a
synthetic envelope with `status = "failed"` and a failure class
`external`. The backgrounded row continues to run until cascade or its own
deadline; the wait simply returns control to the agent.

If the parent is cancelled during a wait, `bridge_cancel_cascade` fires
through the existing path (parametric over kind) and `wait_tool` returns
`status = "cancelled"` with whatever stdout was buffered.

### `cancel_tool`

Arguments:

```json
{
  "tool_call_id": "...",
  "reason": "optional human-readable reason"
}
```

Hook-intercepted. Authorizes through the parent → tool-call edge.

Semantics:

- if the row is already terminal, no-op (return current terminal state)
- otherwise fire `bridge_cancel_cascade` (parametric over Tool kind), which
  signals the in-process executor to cancel: bash subprocess is killed,
  MCP call is aborted via the MCP client cancel path
- the row reaches `.cancelled` through the usual cancel-cascade path
- a `<tool-completion ... status="cancelled">` transcript notification is
  written and a coalesced wake-up is enqueued (the agent's parent request
  is the issuer of the cancel, so the wake-up coalesces with anything
  already in-flight under the same key)

Return:

```json
{
  "tool_call_id": "...",
  "status": "cancelled"
}
```

## Cancellation Policy

R6 hard-codes `cancel_policy = cascade`. The agent surface does not expose
the policy as a per-call argument. The `CancelPolicy = cascade | detach`
vocabulary remains in the Lean model for a future v2 once a concrete caller
needs `detach` (e.g., backgrounded bash writing to a file the user wants to
keep after a session cancel). Until then, parent cancel always cascades.

Two-level cascade is automatic through the parametric bridge. When a
subagent is cancelled (via its own parent's cascade or via direct
`cancel_subagent`), the subagent's request reaches a terminal state; every
backgrounded `AgentToolCall` on that subagent then fires
`bridge_cancel_cascade`. For Subagent kind the cascade sets
`child.interruptRequestedAt`; for Tool kind it signals the executor to
cancel. Both branches flow through the same parametric transition.

The transcript notification for a cascaded tool cancellation carries
`status = "cancelled"` and `reason = "parent_cancelled"` so the agent can
distinguish cascade from explicit `cancel_tool`.

## Recovery Contract

R6 extends `ToolCallLifecycle::recover_all` (#189) with one new
`RecoveryAction` variant:

```lean
inductive RecoveryAction where
  -- existing variants...
  | TerminalizeBackgroundedAsInterrupted
```

Predicate:

```
awaitMode = .background
∧ state = .running
∧ ¬terminal parent.request.state
```

Action on restart:

- mark the row as `.interrupted` (a non-completed terminal state already in
  the vocabulary)
- the captured stdout/stderr buffer is gone (it lived in process memory);
  the transcript notification carries an empty payload with reason
  `interrupted_on_restart`
- the coalesced wake-up is enqueued under
  `background_completion:<parent_session_id>` so the parent session learns
  of the interruption next time it's claimed

This is the v1 simplification: no `resumable` capability bit, no attempt to
re-attach to MCP servers across restart. A future v2 may introduce a
`resumable: bool` on the tool capability and a dispatch on it inside
`TerminalizeBackgroundedAsInterrupted` (the predicate is unchanged; only the
action's effect changes per capability).

If the parent request itself is already terminal at recovery time, the row
is terminalized through the existing #189 stuck-running path (no special
handling — `parent.request.state` already being terminal means the
backgrounded row is logically orphaned).

If the parent behavior's `backgroundable_tool_names` allowlist has changed
between runs such that the row's target tool is no longer authorized, the
row is still terminalized as `.interrupted` (with reason
`tool_not_allowed_at_recovery`); the existence of the row is the
authorization witness for this single recovery action, but the parent
session's next wake-up will not be able to re-create such a row.

## Streaming Output Behavior

v1 buffers in-memory only. A backgrounded tool's stdout/stderr is captured
into an in-memory ring buffer keyed by `tool_call_id`. There is no live
partial-stream document and no composition with `StreamingResponse.Status`
(#190) in v1.

The buffer is bounded by `MAX_BACKGROUND_TOOL_OUTPUT_BYTES = 256 KB` per
stream (stdout and stderr counted separately). On overflow, the ring drops
the oldest bytes and a single `truncated: true` flag is set on the terminal
envelope; the transcript notification mentions truncation.

On terminal, the captured payload is included in the `<tool-completion>`
transcript notification (subject to the same byte cap; the in-line
representation is the same captured-so-far slice) and is the body of any
in-flight `wait_tool`'s envelope.

The peek-while-running tool (`read_tool_output`) is deferred to v1.1,
bundled with R4c's `read_subagent_transcript`. The in-memory buffer is
already maintained from the start, so the v1.1 add is just a tool
registration plus a query against the buffer keyed by `tool_call_id`.

Cross-process streaming, live partial documents, and composition with
`StreamingResponse.Status` are explicitly out of scope.

Backgrounded tool calls interact with compaction (#184): the
`<tool-completion>` notification is a tool-call/result pair just like an
ordinary tool call, so compaction's tool-call-pair preservation policy
applies as-is. The implementation plan should verify this when #184
lands — no additional spec changes expected.

## Hook And Tool Runtime Integration

R6 tools register as native Rig tools when the parent behavior's
`ToolSelectionDocument.backgroundable_tool_names` is non-empty.
`background_tool`, `wait_tool`, and `cancel_tool` all share one
registration site: if at least one allowlisted backgroundable tool exists,
all three meta-tools register.

The hook path generalizes the existing R4 subagent-aware path
(`subagent_tools.rs` becomes `background_tools.rs`; `subagent_completion.rs`
becomes `background_completion.rs`). For ordinary backgrounded tools the
hook must:

- parse and validate `background_tool` args early, including the target
  tool's own argument schema
- check eligibility (capability ∧ allowlist) and budget (B7)
- create a Tool-kind bridge row with `ToolCallLifecycle::new_background_tool`
  (a parametric sibling of `new_subagent`)
- persist `awaitMode = background`, `cancelPolicy = cascade`, no
  `child_request_id`
- launch the in-process executor with cancellation handle wired to
  `bridge_cancel_cascade`
- intercept `wait_tool` before ordinary Rig persistence and observe the
  existing bridge row, returning from it without a second `AgentToolCall`
- intercept `cancel_tool` and fire `bridge_cancel_cascade` on the
  authorized row
- never native-complete a Tool-kind bridge row (B2 prohibition carries)
- project terminal state via parametric `bridge_complete` / `bridge_failure`,
  matching the subagent path's existing projector logic in
  `background_completion.rs`

Queue plumbing:

- queue source string `subagent_completion` renames to
  `background_completion`. Old `subagent_completion` is accepted as a
  back-compat alias for one release; the alias is removed in the release
  after.
- queue key for tool completion: `background_completion:<parent_session_id>`
  (same shape as R4's subagent key; the source field distinguishes the two
  in trace).
- queue policy: `coalesce` (matches R4).

In-flight migration: on the upgrade release that renames the source string,
any `AgentRequest` rows with `metadata.queue.source = "subagent_completion"`
already in the durable store are read by the back-compat alias and treated
as `background_completion`. No data migration is required.

## Out Of Scope

- Cross-deployment backgrounded tools.
- Detach cancel-policy (vocabulary preserved in the model; not exposed in
  v1).
- Live streaming of backgrounded tool output into the response document or
  a separate `BackgroundedToolStream` document. v1 buffers in-memory; v2
  may introduce a streaming document if a concrete caller needs it.
- Cross-restart resume of backgrounded tools (e.g., MCP `resumable`
  capability). v1 always terminalizes as `.interrupted` on restart.
- `read_tool_output` agent-facing peek tool. Deferred to v1.1 bundled with
  R4c's `read_subagent_transcript`.
- Per-session and per-tool-name budgets beyond the per-parent ceiling.
- Token/cost budget propagation across backgrounded tool work.

## Approval Checklist

Before implementation planning, Jack should approve:

- the rename `Proofs/Subagent/` → `Proofs/Background/` and Rust mirrors,
  shipping in the same PR as R6
- the parametric `BridgedState` over `BackgroundedKind = Subagent | Tool`
- the v1 agent surface: `background_tool`, `wait_tool`, `cancel_tool`
- tool capability bit `backgroundable` + operator allowlist
  `ToolSelectionDocument.backgroundable_tool_names`
- per-parent ceiling `MAX_BACKGROUNDED_TOOLS_PER_PARENT = 8` enforced as
  theorem B7
- hard-coded `cancel_policy = cascade` for v1
- automatic two-level cascade through parametric `bridge_cancel_cascade`
- one new `RecoveryAction` variant `TerminalizeBackgroundedAsInterrupted`
- v1 in-memory buffering of stdout/stderr at
  `MAX_BACKGROUND_TOOL_OUTPUT_BYTES = 256 KB`
- queue source rename `subagent_completion` → `background_completion`
  with one-release back-compat alias
- deferral of `read_tool_output`, `detach` cancel-policy, `resumable`
  capability, live streaming, and per-session/per-tool-name budgets
- the implementation plan should sequence execution after #190 and #184
  merge
