# Desktop operator surfaces for background tools, subagents, and interrupts

Date: 2026-05-20

Branch: `design/issue-265-desktop-operator-surfaces`

Tracks issue: https://github.com/sourcenetwork/defra-agent/issues/265

Inputs:

- `docs/superpowers/specs/2026-05-20-feature-matrix-design.md`
- `crates/defra-agent-cli/src/http/liveness.rs`
- `crates/defra-agent/src/native_executor_status.rs`
- `crates/defra-agent/proofs/Proofs/Background/Properties/*.lean`
- `crates/defra-agent/proofs/Proofs/ToolExecution/CancelCause.lean`
- `crates/defra-agent/proofs/Proofs/CrossMachineComposed/ToolTermination.lean`

## TL;DR

Add one desktop "Operations" surface inside the existing chat workspace, not a
new route. The surface is fed by the existing snapshot bridge plus one
operations projection command that joins DefraDB rows with runtime liveness
from `/status`'s `liveness` payload. The frontend continues to subscribe through
`desktop://client-updated`; native executor liveness is refreshed by a bridge
side watcher, not ad hoc React polling.

The six panels are:

1. a backgrounded tools table in the operations rail;
2. an interrupt button beside the composer send button;
3. a parent-to-child lineage tree in the same rail;
4. inline derived cancel-cause badges in transcript tool/request rows;
5. a stuck-work banner plus details list for `expired_processing_count > 0`;
6. a cascade preview dialog before interrupting a parent that owns children.

All panels bind to existing runtime data: `AgentRequest`, `AgentToolCall`,
`AgentResponse`, and liveness fields. `CancelCause` is not persisted today, so
the UI renders a derived cause with source evidence rather than requiring a new
schema field.

## Existing desktop shape

The app currently has three top-level views in
`apps/desktop-tauri/src/App.tsx`: `fleet`, `chat`, and `config`. The chat view
is:

```text
App
  Sidebar
  section.chat-column
    ChatWorkspace
      ChatHeader
      section.chat-workspace
        div.chat-main
          ChatTranscriptPanel
            MessageList
          ChatComposer
```

The bridge already has a snapshot/event loop:

- `desktop_client_snapshot` builds the fleet/chat/config store projection.
- `desktop_session_snapshot(session_id, agent_did, request_id)` builds the
  selected transcript projection.
- `spawn_client_update_task` emits `desktop://client-updated` for store and
  health changes.
- React listens in `apps/desktop-tauri/src/lib/desktop-events.ts` and refreshes
  snapshots in `useDesktopShellEffects`.

This design extends that path. It does not introduce a separate desktop route
or make the browser layer poll raw GraphQL.

## Feature Matrix Tags

Each implementation PR should add a Lean coverage ledger row tagged with the
feature below and `surfaces := [Surface.operatorUi]`. These are the predictable
rows the matrix drift test should expect once the panels land.

| Panel | Feature | `operatorUi` row intent |
|---|---|---|
| Backgrounded tools panel | `background-tools` | `DesktopBackgroundToolsPanelCases` validates the UI projection over `AgentToolCall.await_mode = "background"` plus `active_native_executors`. |
| Interrupt/cancel button | `interrupt-and-cancel` | `DesktopInterruptButtonCases` validates active request detection and `desktop_interrupt_request` dispatch. |
| Subagent lineage view | `background-tools` | `DesktopSubagentLineageCases` validates parent-child projection from request/tool bridge fields. |
| CancelCause surfacing | `interrupt-and-cancel` | `DesktopCancelCauseInlineCases` validates derived cause labels against the `CancelCause` vocabulary. |
| Stuck-tool diagnostics | `request-lifecycle` | `DesktopStuckToolDiagnosticsCases` validates `expired_processing_count` and expired request/tool detail surfacing. |
| Cascade cancel UX | `background-tools` | `DesktopCascadeCancelPreviewCases` validates cascade/detach preview rows before parent interrupt. |

`subagents-cross-deployment.operatorUi` remains out of scope here. The lineage
tree is a same-desktop operator view over existing parent/child request and
bridge rows, not an R5 topology renderer.

## Panel 1: Backgrounded Tools Panel

Feature: `background-tools`, `operatorUi`.

### Visual

Parent component: `apps/desktop-tauri/src/components/operations/OperationsRail.tsx`
mounted by `ActiveChatWorkspace`. Panel component:
`components/operations/BackgroundToolsPanel.tsx`.

```text
Operations
+-- Background Tools -------------------------------------------+
| 4 live / 8 max for req_a17                                    |
| Tool             Age    Parent       Status       Process      |
| summarize_files  02:14  req_a17      running      pid 41812    |
| grep             00:48  req_a17      running      native 902   |
| subagent         05:21  req_a17      background   child req_b91|
| index_repo       07:02  req_a17      deadline +   no executor  |
|                                                              |
| [Open lineage] [Interrupt parent]                            |
+---------------------------------------------------------------+
```

The table is intentionally dense: tool name, age, parent request, lifecycle
status, and process/child linkage are the fields the operator needs while a
turn is still moving.

### Data Source

Tauri command: `desktop_operations_snapshot(request)`.

Runtime liveness fields:

- `liveness.active_native_executors_available`
- `liveness.active_native_executors[]`
- `liveness.active_tool_calls[]`

DefraDB collections:

- `AgentToolCall`: `request_id`, `tool_call_id`, `tool_name`,
  `lifecycle_state`, `status`, `started_at`, `deadline_at`, `await_mode`,
  `cancel_policy`, `child_request_id`, `cancel_cascade_intent_at`,
  `cancel_pending_remote_ack`, `stuck_since`.
- `AgentRequest`: `request_id`, `session_id`, `agent_did`,
  `lifecycle_state`, `status`, `subagent_depth`,
  `caused_by_parent_request_id`, `caused_by_parent_tool_call_id`.

Join rule:

- primary rows are `AgentToolCall.await_mode = "background"` and
  non-terminal `lifecycle_state`;
- `active_tool_calls` supplies live age/deadline when the row is still running;
- `active_native_executors` supplies PID/argv for native executors when the
  runtime reports it. `NativeExecutorStatus` does not carry `request_id`, so
  process display is best-effort by `(tool_name, started_at)` and never the
  only source for parent/status.

### Actions

| Action | Backend effect |
|---|---|
| Open lineage | Selects the Lineage tab and calls `desktop_list_subagent_tree` with the parent request id. No mutation. |
| Open parent session | Selects the session already present in the desktop snapshot. No mutation. |
| Copy request/tool ids | Clipboard only. No backend effect. |
| Interrupt parent | Opens the cascade preview for the parent request. Confirmation calls `desktop_interrupt_request`. |

Direct "kill this one background tool" is not in this design. The existing
runtime has agent-facing `cancel_tool`, but there is no desktop bridge command
or public runtime API for per-tool UI cancellation. The desktop interrupt path
therefore enters at `AgentRequest.interrupt_requested_at` and lets existing
cascade/termination machinery do the work.

### Options And Recommendation

| Option | Tradeoff |
|---|---|
| Table in the operations rail | Recommended. It matches operational scanning and fits the current chat workspace without route churn. |
| Inline transcript cards only | Good local context, poor for multiple background tools and stuck rows. |
| Dedicated route | More room, but splits the operator away from the active chat where interrupts happen. |

Recommendation: table in the shared operations rail.

## Panel 2: Interrupt/Cancel Button

Feature: `interrupt-and-cancel`, `operatorUi`.

### Visual

Parent component: `apps/desktop-tauri/src/components/chat/ChatComposer.tsx`.
New child component: `components/chat/InterruptButton.tsx`.

```text
+-- composer ---------------------------------------------------+
| Message the selected agent                                    |
|                                                               |
| Turn still streaming - peers 1/1         [Interrupt] [Send]   |
+---------------------------------------------------------------+
```

When no turn is active, the button is hidden. When a turn is active but no
request id is known, it is disabled with "Waiting for request observation".

### Data Source

Existing frontend projection:

- `projectChatShell(...).workflow`
- `projectChatShell(...).activeRequestId`
- `DesktopSessionSnapshot.turnState`
- `DesktopSessionSnapshot.latestRequestId`
- `ConversationSummary.latestRequestId`

Tauri commands:

- `desktop_preview_interrupt_cascade` when the active request has or may have
  children;
- `desktop_interrupt_request` when the user confirms.

DefraDB collections:

- `AgentRequest.interrupt_requested_at` is the backend latch.
- `AgentResponse.interrupted_at` is stamped later by the daemon interrupt flow.

### Actions

| Action | Backend effect |
|---|---|
| Click Interrupt with no live children | Calls `desktop_interrupt_request({ requestId, cause: "userCancelled", cascade: false })`. The bridge latches `AgentRequest.interrupt_requested_at`. |
| Click Interrupt with live children | Opens the cascade preview dialog. Confirmation calls `desktop_interrupt_request({ requestId, cause: "userCancelled", cascade: true, expectedPreviewSignature })`. |
| Press Escape while composer focused | Same as clicking Interrupt, but only after the request id is observed. |

The operator can only create `CancelCause.userCancelled`. `deadline` and
`interrupted` are derived runtime causes. A free cause selector would imply the
operator can truthfully mark a deadline or parent-interrupt cause; the runtime
does not expose that mutation, so the button does not offer it.

### Options And Recommendation

| Option | Tradeoff |
|---|---|
| Composer footer button | Recommended. It sits where the operator is already deciding whether the turn may continue. |
| Header button | Always visible, but detached from the current turn's send/blocked state. |
| Transcript inline button on the pending turn | High context, but disappears off-screen during long transcripts. |

Recommendation: composer footer button plus Escape shortcut while the composer
or transcript has focus.

## Panel 3: Subagent Lineage View

Feature: `background-tools`, `operatorUi`.

### Visual

Parent component: `OperationsRail`. Panel component:
`components/operations/SubagentLineagePanel.tsx`.

```text
Lineage
req_a17  processing  parent turn
|- tool bg-91  background/cascade  running
|  `- req_b91  processing  amy-code
|     |- tool bg-44  background/detach  running
|     |  `- req_c44  processing  amy-review
|     `- tool fg-10  foreground/cascade  completed
`- tool bg-03  background/cascade  cancelled
   `- req_d03  interrupted
```

Nodes show request id, lifecycle state, behavior when known, and the bridge
tool's `await_mode` / `cancel_policy`.

### Data Source

Tauri command: `desktop_list_subagent_tree(request)`.

Parameters:

```ts
type DesktopListSubagentTreeRequest = {
  rootRequestId: string;
  agentDid?: string | null;
  includeTerminal?: boolean;
  maxDepth?: number;
};
```

Return shape:

```ts
type SubagentTreeView = {
  rootRequestId: string;
  nodes: SubagentNodeView[];
  edges: SubagentEdgeView[];
  truncated: boolean;
};

type SubagentNodeView = {
  requestId: string;
  sessionId?: string | null;
  agentDid?: string | null;
  behaviorId?: string | null;
  lifecycleState?: string | null;
  status?: string | null;
  subagentDepth?: number | null;
  causedByParentRequestId?: string | null;
  causedByParentToolCallId?: string | null;
};

type SubagentEdgeView = {
  parentRequestId: string;
  childRequestId: string;
  parentToolCallId?: string | null;
  toolName?: string | null;
  awaitMode?: "foreground" | "background" | string | null;
  cancelPolicy?: "cascade" | "detach" | string | null;
  lifecycleState?: string | null;
};
```

DefraDB collections:

- `AgentRequest`: `request_id`, `session_id`, `agent_did`, `behavior_id`,
  `status`, `lifecycle_state`, `subagent_depth`,
  `caused_by_parent_request_id`, `caused_by_parent_tool_call_id`.
- `AgentToolCall`: `request_id`, `tool_call_id`, `tool_name`,
  `lifecycle_state`, `await_mode`, `cancel_policy`, `child_request_id`.

The command walks children by `AgentRequest.caused_by_parent_request_id` and
cross-checks bridge edges by `AgentToolCall.child_request_id`.

### Actions

| Action | Backend effect |
|---|---|
| Select node | Focuses details in the rail and can select the matching session if present in `DeploymentView.conversations`. No mutation. |
| Show live only | Re-runs `desktop_list_subagent_tree` with `includeTerminal=false`. No mutation. |
| Interrupt selected request | Runs the same preview/interrupt flow as the composer button, rooted at that request. |
| Copy graph JSON | Serializes `SubagentTreeView` for debugging. No mutation. |

### Options And Recommendation

| Option | Tradeoff |
|---|---|
| Rail tree | Recommended. It shares space with background tools and cascade preview and can stay open while reading transcript. |
| Transcript-adjacent nested cards | Good for one child, poor beyond depth 1. |
| Dedicated graph route | More room, but too heavy for the same-deployment tree and conflicts with the R5 topology icebox. |

Recommendation: rail tree with live-only filtering and node details.

## Panel 4: CancelCause Surfacing

Feature: `interrupt-and-cancel`, `operatorUi`.

### Visual

Parent component: `apps/desktop-tauri/src/components/Transcript.tsx`.
New child components:

- `components/transcript/CancelCauseBadge.tsx`
- `components/transcript/CancelCauseDetails.tsx`

```text
Tool Calls
  * read_file              completed      View
  * background_tool        cancelled      user cancelled - View
      Cause
      userCancelled
      Evidence: parent req_a17 interrupt_requested_at 2026-05-20T...

assistant
  Partial answer...
  interrupted - user cancelled
```

The badge appears on the cancelled tool call row and on an interrupted
assistant turn when `AgentResponse.interrupted_at` is present.

### Data Source

Existing Tauri command extended:

```ts
desktop_session_snapshot(
  sessionId: string,
  agentDid?: string | null,
  requestId?: string | null
) -> DesktopSessionSnapshot | null
```

Return additions:

```ts
type DerivedCancelCauseView = {
  cause: "userCancelled" | "interrupted" | "deadline" | "unknown";
  source:
    | "requestInterrupt"
    | "parentCascade"
    | "deadline"
    | "toolLifecycle"
    | "responseInterruptedAt"
    | "unresolved";
  confidence: "direct" | "derived";
  at?: string | null;
  evidence: string[];
};

type RenderedToolCallView = {
  // existing fields...
  lifecycleState?: string | null;
  deadlineAt?: string | null;
  cancelCause?: DerivedCancelCauseView | null;
};

type ResponseView = {
  // existing fields...
  cancelCause?: DerivedCancelCauseView | null;
};
```

DefraDB collections:

- `AgentRequest`: `request_id`, `lifecycle_state`, `status`,
  `deadline`, `interrupt_requested_at`, `caused_by_parent_request_id`.
- `AgentResponse`: `request_id`, `status`, `error_message`,
  `interrupted_at`, `completed_at`.
- `AgentToolCall`: `request_id`, `tool_call_id`, `lifecycle_state`,
  `status`, `deadline_at`, `completed_at`, `cancel_policy`,
  `child_request_id`.

Cause derivation:

- `deadline`: `AgentToolCall.lifecycle_state = "timedOut"` or expired
  request/tool deadline evidence.
- `interrupted`: child request/tool was cancelled because an ancestor was
  interrupted or a bridge projected child `.interrupted` to parent
  `.cancelled`.
- `userCancelled`: root request has `interrupt_requested_at` and no parent
  cascade evidence.
- `unknown`: cancelled terminal row lacks enough evidence.

The UI must label these as derived evidence. The current schema has no
persisted `AgentToolCall.cancel_cause` field, and this design does not add one.

### Actions

| Action | Backend effect |
|---|---|
| Expand cause details | No mutation. Shows evidence rows and timestamps. |
| Open lineage from cause | Calls `desktop_list_subagent_tree` for the nearest request. No mutation. |
| Copy evidence | Clipboard only. No backend effect. |

### Options And Recommendation

| Option | Tradeoff |
|---|---|
| Inline badge plus disclosure | Recommended. The answer to "why was this stopped?" lives where the stop appears. |
| Operations rail only | Cleaner transcript, but forces the operator to correlate ids manually. |
| Toast/notification | Too transient for audit/debugging. |

Recommendation: inline badge with expandable evidence; rail details may mirror
the selected cause later, but the transcript owns first display.

## Panel 5: Stuck-Tool Diagnostics

Feature: `request-lifecycle`, `operatorUi`.

### Visual

Parent components:

- banner: `components/operations/StuckWorkBanner.tsx`, mounted above
  `ChatTranscriptPanel` inside `ActiveChatWorkspace`;
- detail list: `components/operations/StuckWorkPanel.tsx`, inside
  `OperationsRail`.

```text
+-- Warning: 2 active requests are past deadline ----------------+
| req_a17 is 03:12 past deadline; latest tool index_repo running |
| [View diagnostics] [Interrupt request] [Dismiss]               |
+---------------------------------------------------------------+

Stuck Work
Request     Past deadline  Last progress  Tool          Action
req_a17     03:12          07:02          index_repo    Interrupt
req_b91     00:41          00:41          summarize     Open
```

### Data Source

Tauri command: `desktop_operations_snapshot(request)`.

Runtime liveness fields:

- `liveness.expired_processing_count`
- `liveness.requests[]` with `deadline_expired`, `deadline_age_ms`,
  `last_progress_age_ms`, `subagent_depth`, `caused_by_parent_request_id`
- `liveness.active_tool_calls[]` with `deadline_expired`, `running_age_ms`

DefraDB collections:

- `AgentRequest`: `request_id`, `session_id`, `agent_did`, `status`,
  `lifecycle_state`, `deadline`, `claimed_at`, `failure_reason`,
  `interrupt_requested_at`.
- `AgentToolCall`: `request_id`, `tool_call_id`, `tool_name`,
  `lifecycle_state`, `started_at`, `deadline_at`, `stuck_since`,
  `cancel_pending_remote_ack`, `unclaimed_deadline_at`.

### Actions

| Action | Backend effect |
|---|---|
| View diagnostics | Opens the Stuck Work tab in `OperationsRail`. No mutation. |
| Interrupt request | Opens cascade preview, then calls `desktop_interrupt_request` on confirm. |
| Dismiss | Local UI suppression by diagnostic signature. No backend effect. |
| Refresh now | Calls `desktop_operations_snapshot`. No mutation. |

Auto-clear rule:

- The banner appears when `expired_processing_count > 0` or any projected
  stuck diagnostic has `stuck_since` / `cancel_pending_remote_ack`.
- It auto-clears after two consecutive operations snapshots with
  `expired_processing_count = 0` and no stuck diagnostics.
- A dismissed banner reappears when the diagnostic signature changes
  `(request ids, tool ids, deadline ages bucketed by minute)`.

### Options And Recommendation

| Option | Tradeoff |
|---|---|
| Banner plus rail details | Recommended. The banner makes the condition hard to miss; details stay out of the transcript. |
| Rail warning only | Too easy to miss when the rail is collapsed. |
| System notification | Too noisy and not correlated with the active session. |

Recommendation: banner for presence, rail for diagnosis.

## Panel 6: Cascade Cancel UX

Feature: `background-tools`, `operatorUi`.

### Visual

Parent component: `components/operations/CascadeCancelDialog.tsx`, launched
from `InterruptButton`, `BackgroundToolsPanel`, or `SubagentLineagePanel`.

```text
Interrupt parent request?

req_a17  streaming  current turn

Will request interrupt by cascade
  req_b91  processing  background/cascade  summarize child
  req_d03  claimed     background/cascade  index child

Will continue detached
  req_c44  processing  background/detach   review child

Already terminal
  req_e12  completed   foreground/cascade

[Cancel] [Interrupt parent and cascade]
```

The confirm copy says "request interrupt" rather than "kill" because the
backend latches `interrupt_requested_at`; child terminality is completed by the
runtime observers/recovery path.

### Data Source

Tauri command: `desktop_preview_interrupt_cascade(request)`.

Parameters:

```ts
type DesktopPreviewInterruptCascadeRequest = {
  requestId: string;
  agentDid?: string | null;
  includeTerminal?: boolean;
};
```

Return shape:

```ts
type CascadeCancelPreview = {
  rootRequestId: string;
  previewSignature: string;
  rootState?: string | null;
  willInterrupt: CascadeAffectedRequest[];
  willDetach: CascadeAffectedRequest[];
  alreadyTerminal: CascadeAffectedRequest[];
  unknownPolicy: CascadeAffectedRequest[];
};

type CascadeAffectedRequest = {
  requestId: string;
  sessionId?: string | null;
  behaviorId?: string | null;
  lifecycleState?: string | null;
  parentRequestId?: string | null;
  parentToolCallId?: string | null;
  toolName?: string | null;
  awaitMode?: string | null;
  cancelPolicy?: string | null;
};
```

DefraDB collections:

- `AgentRequest` parent lineage fields.
- `AgentToolCall.child_request_id`, `await_mode`, `cancel_policy`,
  `lifecycle_state`.

The preview classifies descendants by the bridge row nearest their parent:

- `cancel_policy = "cascade"` and child non-terminal: `willInterrupt`;
- `cancel_policy = "detach"` and child non-terminal: `willDetach`;
- terminal child request/tool state: `alreadyTerminal`;
- missing bridge policy or inconsistent link: `unknownPolicy`.

### Actions

| Action | Backend effect |
|---|---|
| Confirm cascade interrupt | Calls `desktop_interrupt_request({ requestId, cause: "userCancelled", cascade: true, expectedPreviewSignature })`. |
| Cancel dialog | No mutation. |
| Open child row | Selects the lineage node. No mutation. |

`expectedPreviewSignature` protects against confirming a stale preview. The
bridge recomputes the preview immediately before latching the interrupt; if the
signature changed, it returns `stalePreview: true` and the UI redraws the dialog.

### Options And Recommendation

| Option | Tradeoff |
|---|---|
| Confirmation dialog with grouped affected lists | Recommended. It is explicit, interrupt-safe, and does not require a large graph layout. |
| Inline expandable preview under the Interrupt button | Fewer clicks, but cramped and easy to miss for multi-child cascades. |
| Full lineage tree as confirmation | Richest context, but too heavy for the moment of interruption. |

Recommendation: modal confirmation with grouped lists and links into the
lineage rail.

## Shared Data Layer

### Recommendation

Use the existing snapshot bridge and Tauri event subscription as the shared
data layer:

1. Add an operations projection in the Rust bridge.
2. Extend `spawn_client_update_task` with a low-cost operations watcher that
   emits `desktop://client-updated` with `reason: "operations"` when the
   liveness signature changes.
3. Keep React on the existing listener path in
   `listenToDesktopClientUpdates`; `useDesktopShellEffects` refreshes
   operations snapshots the same way it refreshes client/session snapshots.

This is not pure frontend polling. The only polling is bridge-side, where it
can be bounded, shared, and turned into the same event stream the desktop
already understands.

### Why Not A New Frontend Poller

The current desktop shell already centralizes refresh, selected-agent scoping,
and store/health coalescing. A second React timer for liveness would compete
with that logic and would still need session refresh coordination after
interrupts. Keeping the bridge as the observer preserves one refresh path.

### Why Not `bridge_runner`

`bridge_runner` is a live-test fixture and HTTP facade around the bridge code.
It should mirror the new operations commands for tests, but it is not a
product data plane. It starts a fixture, owns stdin shutdown, and is shaped for
Playwright/live smoke tests rather than long-lived desktop UI state.

### Operations Snapshot Type

```ts
type DesktopOperationsSnapshotRequest = {
  agentDid?: string | null;
  rootRequestId?: string | null;
  includeTerminal?: boolean;
};

type DesktopOperationsSnapshot = {
  fetchedAt: string;
  agentDid?: string | null;
  liveness?: RuntimeLivenessView | null;
  livenessUnavailableReason?: string | null;
  backgroundedTools: BackgroundedToolView[];
  stuckDiagnostics: StuckWorkDiagnosticView[];
  lineage?: SubagentTreeView | null;
};

type RuntimeLivenessView = {
  activeRequestIds: string[];
  expiredProcessingCount: number;
  requests: ActiveRequestView[];
  activeToolCalls: ActiveToolCallView[];
  activeNativeExecutorsAvailable: boolean;
  activeNativeExecutors: NativeExecutorStatusView[];
};

type ActiveRequestView = {
  requestId: string;
  claimedAt?: string | null;
  deadline?: string | null;
  deadlineExpired: boolean;
  deadlineAgeMs?: number | null;
  lastProgressAgeMs: number;
  subagentDepth: number;
  causedByParentRequestId?: string | null;
  causedByTriggerKind?: string | null;
};

type ActiveToolCallView = {
  requestId: string;
  toolCallId: string;
  toolName: string;
  startedAt?: string | null;
  deadlineAt?: string | null;
  awaitMode?: string | null;
  runningAgeMs: number;
  deadlineExpired: boolean;
};

type NativeExecutorStatusView = {
  id: number;
  pid: number;
  argv0: string;
  toolName?: string | null;
  startedAt: string;
  ageMs: number;
};

type BackgroundedToolView = {
  requestId: string;
  toolCallId: string;
  toolName: string;
  lifecycleState?: string | null;
  status?: string | null;
  startedAt?: string | null;
  ageMs?: number | null;
  deadlineAt?: string | null;
  deadlineExpired: boolean;
  awaitMode?: string | null;
  cancelPolicy?: string | null;
  childRequestId?: string | null;
  nativeExecutor?: NativeExecutorStatusView | null;
};

type StuckWorkDiagnosticView = {
  requestId: string;
  sessionId?: string | null;
  severity: "warning" | "critical";
  reason:
    | "expiredProcessing"
    | "expiredTool"
    | "stuckTool"
    | "pendingRemoteCancelAck";
  deadlineAgeMs?: number | null;
  lastProgressAgeMs?: number | null;
  toolCallId?: string | null;
  toolName?: string | null;
  stuckSince?: string | null;
};
```

`RuntimeLivenessView` mirrors `RuntimeLivenessSnapshot` from
`crates/defra-agent-cli/src/http/liveness.rs` with camelCase field names.

## New Tauri Commands

All commands live under `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands`
with pure bridge helpers under `bridge/commands` or `bridge/snapshot`.

| Command | Parameters | Return | Notes |
|---|---|---|---|
| `desktop_operations_snapshot` | `{ agentDid?: string | null, rootRequestId?: string | null, includeTerminal?: boolean }` | `DesktopOperationsSnapshot` | Joins liveness, background rows, stuck diagnostics, and optional lineage. No mutation. |
| `desktop_list_subagent_tree` | `{ rootRequestId: string, agentDid?: string | null, includeTerminal?: boolean, maxDepth?: number }` | `SubagentTreeView` | Reads `AgentRequest` and `AgentToolCall` lineage fields. No mutation. |
| `desktop_preview_interrupt_cascade` | `{ requestId: string, agentDid?: string | null, includeTerminal?: boolean }` | `CascadeCancelPreview` | Classifies descendants into cascade/detach/terminal/unknown. No mutation. |
| `desktop_interrupt_request` | `{ requestId: string, cause: "userCancelled", cascade: boolean, expectedPreviewSignature?: string | null }` | `InterruptRequestResult` | Latches `AgentRequest.interrupt_requested_at`, refreshes store, emits `desktop://client-updated` reason `interrupt`. |

`InterruptRequestResult`:

```ts
type InterruptRequestResult = {
  requestId: string;
  accepted: boolean;
  interruptRequestedAt?: string | null;
  alreadyInterrupted: boolean;
  stalePreview: boolean;
  preview?: CascadeCancelPreview | null;
};
```

Existing command extensions:

| Command | Extension |
|---|---|
| `desktop_session_snapshot(sessionId, agentDid, requestId)` | Add derived cancel-cause fields to `ResponseView` and `RenderedToolCallView`. |
| `desktop_client_snapshot()` | No required shape change for v1. It may include an `operationsSummary` later, but panels should use `desktop_operations_snapshot` for liveness. |

Remote behavior:

- For a local runtime, `desktop_interrupt_request` can call
  `defra_agent::interrupt_request(node, request_id)`.
- For a remote GraphQL peer, the bridge performs the same existing data-plane
  mutation against `AgentRequest.interrupt_requested_at`.
- If the selected deployment has no reachable liveness `/status` endpoint,
  operations snapshot still returns DefraDB-derived rows and sets
  `livenessUnavailableReason`; `active_native_executors_available` is false.

## Component Decomposition

New frontend files:

```text
apps/desktop-tauri/src/components/operations/
  OperationsRail.tsx
  BackgroundToolsPanel.tsx
  SubagentLineagePanel.tsx
  StuckWorkBanner.tsx
  StuckWorkPanel.tsx
  CascadeCancelDialog.tsx
  operationsFormatting.ts

apps/desktop-tauri/src/components/chat/
  InterruptButton.tsx

apps/desktop-tauri/src/components/transcript/
  CancelCauseBadge.tsx
  CancelCauseDetails.tsx

apps/desktop-tauri/src/hooks/
  useDesktopOperations.ts

apps/desktop-tauri/src/lib/
  desktop-operations.ts
  types/operations.ts
```

Mounting:

```text
ActiveChatWorkspace
  ChatHeader
  StuckWorkBanner
  section.chat-workspace
    div.chat-main
      ChatTranscriptPanel
      ChatComposer
        InterruptButton
    OperationsRail
      BackgroundToolsPanel
      SubagentLineagePanel
      StuckWorkPanel
  CascadeCancelDialog
```

The rail should be collapsible. Collapsed state still allows the stuck banner
and interrupt button to work.

## Out Of Scope

- Implementing the React panels or Rust commands in this PR.
- Adding a persisted `cancel_cause` field or changing runtime transition
  semantics. Cause rendering is derived from existing rows.
- Direct per-tool desktop cancellation. The UI cancellation entry point is
  request interrupt plus existing cascade behavior.
- R5 cross-deployment topology rendering.
- Mobile or web targets.
- Reworking the top-level `fleet` / `chat` / `config` navigation.

## Implementation Phasing

1. Shared operations projection: `desktop_operations_snapshot`, liveness
   watcher, TypeScript operation types. This unblocks Backgrounded Tools and
   Stuck Work.
2. Interrupt button: `desktop_interrupt_request` plus composer integration.
   This can ship in parallel with CancelCause rendering because it only needs
   active request detection.
3. CancelCause inline rendering: extend `desktop_session_snapshot` and
   `Transcript.tsx`. It can ship independently of the operations rail.
4. Backgrounded Tools and Stuck Work panels: both depend on the operations
   projection and can be implemented in parallel once that projection exists.
5. Subagent Lineage: depends on request/tool lineage fields in the operations
   projection or `desktop_list_subagent_tree`.
6. Cascade Cancel UX: depends on `desktop_preview_interrupt_cascade` and the
   lineage classifier. It should follow the interrupt button and lineage query.

The minimum useful sequence is: data projection, interrupt button, cascade
preview. The most parallel sequence is: interrupt button and CancelCause first,
operations projection next, then background/stuck/lineage/cascade panels.
