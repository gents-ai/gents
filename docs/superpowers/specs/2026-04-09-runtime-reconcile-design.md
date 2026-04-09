# Runtime Reconciliation

Date: 2026-04-09

## Problem

The runtime resolves all behavior configuration once at startup. `resolve_tool_surfaces()` runs once, each `BehaviorDaemon` builds its rig `Agent` once, and the scheduler captures tool surfaces at construction. Changing an AgentBehavior's prompt, swapping a ToolSelection, or updating an InferenceProfile requires a full process restart.

The runtime is document-driven but not document-reactive. The control plane documents define desired state, but the runtime only reads them once. This makes operational iteration slow and prevents the system from converging on desired state without manual restarts.

## Design Decisions

- **Collection-scoped, event-bus driven control watchers.** The control plane subscribes directly to the DefraDB collections that define runtime configuration (`AgentBehavior`, `ToolSelection`, `InferenceProfile`, `InferenceBackend`, `AgentPrincipal`) rather than consuming the generic request watcher path. Reconcile is triggered by those collection-scoped update notifications.
- **Debounce coalesces rapid updates.** A 5-second quiet period after the last relevant event before reconcile fires. This handles multi-document updates (e.g., changing an AgentBehavior to point to a new ToolSelection while also updating the InferenceProfile) arriving in quick succession, including with P2P gossip jitter.
- **Full re-resolve, not incremental tracking.** The reconcile cycle does not track which specific documents changed. It re-resolves the complete behavior set from DB and diffs against the current generation. The cost is trivial (a handful of GraphQL queries every 5+ seconds at most) and the simplicity is worth it.
- **Resolved state and active state are distinct.** The control watcher produces a fully resolved desired snapshot. The supervisor owns executor/channel installation and publishes the active generation only after the new dispatch table exists. Router and scheduler consume only active snapshots.
- **In-flight requests finish on their generation.** A reconcile does not interrupt running requests. New requests pick up the latest generation. Two generations may briefly coexist. This is safe because the old generation was valid when the request started.
- **Generation binding is per-request, not per-session.** Sessions remain pinned to `behavior_id`, not to a historical generation. Each newly dispatched request binds to the latest active generation for that behavior. Old generations are retained only until in-flight requests drain. Session history and compaction entries may therefore span generations by design.
- **Full behavior set reconciliation.** Adding behaviors, removing/disabling behaviors, updating behavior configs, and changing the principal's `default_behavior_id` are all reconcile scope.
- **AgentPrincipal identity is immutable.** The `agent_did` never changes. Only `default_behavior_id` (and potentially `display_name`) can change on the principal document.
- **An empty active generation is valid.** If reconcile yields zero runnable behaviors, the process stays alive, publishes an active snapshot with empty dispatchers, rejects new work, and waits for a later reconcile to make behaviors runnable again.
- **Expose reconcile state in DefraDB.** Runtime reconciliation writes an `AgentRuntime` document keyed by `agent_did` so operators can query the current process state, reconcile phase, active generation, and last reconcile outcome directly from DefraDB while debugging.
- **This slice starts with a Lean model and conformance tests.** Runtime reconciliation introduces enough concurrent state machinery that we should specify the reconcile state machine formally before wiring the Rust runtime. The initial implementation slice is: Lean model -> Rust conformance/regression tests -> runtime code.

## Reconcile State Machine

```
Idle -> Debouncing -> Resolving -> Diffing -> Applying -> Idle
```

**Idle:** Waiting for event bus updates on watched collections (AgentBehavior, ToolSelection, InferenceProfile, InferenceBackend, AgentPrincipal). Zero CPU cost -- blocked on the event bus subscription.

**Debouncing:** A relevant event was received. A 5-second timer is running. Each new relevant event resets the timer. No DB queries during this phase.

**Resolving:** The debounce timer expired. The watcher queries DefraDB for the full behavior set:
1. Query `AgentPrincipal` for `default_behavior_id`
2. Query all `AgentBehavior` documents for this principal's `agent_did`
3. For each behavior, resolve its `ToolSelection`, optional `InferenceProfile`, and required `InferenceBackend`. `InferenceBackend.endpoint` is consumed as an OpenAI-compatible API base URL, including the `/v1` path segment.
4. Partition into runnable vs unrunnable (same rules as startup -- missing or unhealthy backend = unrunnable)
5. Build `BehaviorConfig` + `ToolSurface` per runnable behavior

**Diffing:** Compare the newly resolved desired snapshot against the last applied resolved snapshot using structural equality. If identical, transition to Idle (no-op). Log at `debug` level.

**Applying:** The desired snapshot differs. Hand it to the supervisor. The supervisor creates any new request channels, starts any added or updated executors, builds the next active snapshot, increments the generation counter, publishes the active snapshot via `watch::Sender`, then retires superseded senders/executors. Log at `info` level with behavior add/remove/update counts. Transition to Idle.

**Error handling:** Errors during Resolving or Applying are logged at `error` level and the machine returns to Idle. The next document change will trigger another cycle. Reconcile failures are transient -- they never poison the running generation.

## Formal Specification First

Before implementing runtime reconciliation in Rust, model the reconcile machinery in Lean.

Scope of the formal model:
1. Snapshot generations are monotonic
2. Reconcile publishes either the previous active snapshot (no-op) or one fully applied next active snapshot
3. Router and scheduler never observe a generation before its dispatch channels exist
4. Router, supervisor, and scheduler observe the same published generation ordering
5. In-flight work continues on the generation it started with
6. New work is admitted only against the latest published generation
7. Reconcile failure does not destroy the previously active generation
8. Session continuity is behavior-scoped while generation binding is request-scoped

This is not a replacement for the existing request/process lifecycle proofs. It is an additional runtime-coordination model for generation publication and consumer visibility.

Implementation order:
1. Add Lean definitions and invariants for the reconcile machine
2. Add Rust conformance/regression tests for generation publication and consumer behavior
3. Implement the control watcher, snapshot publication, and runtime consumers

## ResolvedSnapshot And ActiveRuntimeSnapshot

```rust
struct ResolvedSnapshot {
    default_behavior_id: String,
    behaviors: HashMap<String, Arc<BehaviorConfig>>,
    tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    unavailable_behaviors: HashMap<String, String>,
}

struct ActiveRuntimeSnapshot {
    generation: u64,
    default_behavior_id: String,
    behaviors: HashMap<String, Arc<BehaviorConfig>>,
    tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    unavailable_behaviors: HashMap<String, String>,
    dispatchers: HashMap<String, mpsc::Sender<AgentRequest>>,
}
```

`ResolvedSnapshot` is pure desired runtime state from DefraDB. It is what the control watcher computes and diffs.

`ActiveRuntimeSnapshot` is what the router and scheduler consume. It includes the installed dispatch table for the currently active generation.

Generation is a monotonic counter starting at 1 (the initial startup resolution + install). The startup path produces the first active snapshot using the same resolution logic. Reconcile and startup share one code path for resolution.

The active snapshot is published through `watch::Sender<Arc<ActiveRuntimeSnapshot>>`. Consumers hold `watch::Receiver<Arc<ActiveRuntimeSnapshot>>` and check for new generations at their natural decision points.

The control watcher sends `ResolvedSnapshot` proposals to the supervisor over an internal channel. It does not publish directly to router or scheduler.

## Runtime Observability Document

`AgentRuntime` is a single-document operational view for a running principal:

```graphql
type AgentRuntime {
  agent_did: String
  process_state: String
  reconcile_phase: String
  active_generation: Int
  default_behavior_id: String
  runnable_behavior_count: Int
  unavailable_behavior_count: Int
  last_reconcile_result: String
  last_reconcile_error: String
  last_reconcile_completed_at: String
  updated_at: String
}
```

`reconcile_phase` mirrors the formal machine phase (`idle`, `debouncing`, `resolving`, `diffing`, `applying`). `process_state` mirrors the runtime lifecycle (`recovering`, `ready`, `shuttingDown`, `shutdown`). This document is observability-only; it does not participate in reconciliation decisions.

## Control Watcher

A new `ControlWatcher` is spawned in `run_agent()` alongside the router and supervisor. It:

1. Subscribes to the DefraDB event bus for update events, scoped to the collections that define runtime configuration
2. Listens to `AgentBehavior`, `ToolSelection`, `InferenceProfile`, `InferenceBackend`, and `AgentPrincipal` update streams directly, rather than trying to infer collection membership from request events
3. Unlike `DefraWatcher` which only processes P2P-relayed events (`is_relay`), the control watcher reacts to both local and P2P updates since configuration changes may arrive via either path
4. On a relevant event, resets the 5-second debounce timer
5. On debounce expiry, runs the resolve/diff cycle
6. Sends the new `ResolvedSnapshot` to the supervisor when configuration has changed
7. The supervisor is then responsible for applying and publishing the next active generation

The watcher is cancellation-aware via the existing `CancellationToken` and shuts down cleanly with the rest of the runtime.

## Session And Generation Semantics

- `AgentSession` and `AgentConversation` remain pinned to `behavior_id`, not to a specific runtime generation.
- A request binds to a generation only when the router dispatches it to an active executor.
- A later request on the same session intentionally uses the latest active generation for that behavior.
- A change to the principal's `default_behavior_id` affects only requests that do not already carry a session-bound behavior.
- Old generations are retained only for in-flight requests. Idle sessions do not keep historical generations alive.
- If the session's behavior is removed or becomes unavailable, the next request on that session is rejected through the normal behavior-unavailable path.
- Mixed-generation session history is allowed by design. Prompt changes, tool-surface changes, and inference-profile changes can take effect between turns on the same session.

## Consumer Contracts

### Router

The router holds a `watch::Receiver<Arc<ActiveRuntimeSnapshot>>`. At the top of its dispatch loop (before processing the next request from the watcher), it checks `has_changed()`. If a new snapshot is available, it swaps its current active generation, including `default_behavior_id`, `unavailable_behaviors`, and the already-installed dispatch map.

Requests for behaviors that no longer exist get the same "behavior unavailable" error response path that already exists for unrunnable behaviors.

### Supervisor

The supervisor is the sole owner of executor tasks and request channels. It consumes `ResolvedSnapshot` proposals from the control watcher and performs set arithmetic against the currently active generation:

**Behavior added** (in new snapshot, not running): Spawn a new executor. Create a new request channel. The next published active snapshot will start routing requests to it.

**Behavior removed** (running, not in new snapshot): After publishing the next active generation without that behavior, drop the old `mpsc::Sender` for that behavior. The executor's receiver yields `None`, causing it to exit cleanly after its in-flight work drains. The supervisor reaps the task normally.

**Behavior updated** (in both, config or tool surface differs): Treat as add-new + publish + retire-old. Create a fresh channel, spawn a fresh executor with the new config, include that sender in the next active snapshot, publish it, then drop the old sender. Requests queue in the new channel during the brief transition. In-flight requests on the old executor finish on their generation.

**Publication rule:** A generation is not visible to router or scheduler until its dispatch map has been installed. This is the core ownership invariant that prevents publish-before-dispatch races.

### Scheduler

The scheduler holds a `watch::Receiver<Arc<ActiveRuntimeSnapshot>>`. At each tick cycle (60 seconds), before scanning for due tasks, it checks `has_changed()`. If a new snapshot is available, it swaps its local behavior and tool surface references.

The scheduler already builds a fresh rig `Agent` per task execution, so it naturally picks up new configs on the next run. A running task finishes with whatever it started with. If a scheduled task references a `behavior_id` absent from the snapshot, it records `last_error` as it does today for unrunnable behaviors.

## Startup Convergence

The initial startup resolution is refactored to produce the first `ResolvedSnapshot`, then install and publish the first `ActiveRuntimeSnapshot` (generation 1) using the same resolution code path that reconcile uses. This eliminates duplicate resolution logic and guarantees that startup and reconcile produce structurally identical resolved snapshots.

The startup sequence becomes:
1. Boot DefraDB, establish principal
2. Resolve initial `ResolvedSnapshot`
3. Install initial request channels/executors
4. Publish initial `ActiveRuntimeSnapshot` (generation 1)
5. Spawn control watcher, router, supervisor, scheduler
6. Control watcher enters Idle, waiting for document changes

## Out of Scope

- MCP service-registry-triggered rebuilds (transient health changes handled by error returns at call time)
- Implicit reconcile on every document write without debounce
- Changes to request lifecycle or session semantics outside the reconcile generation/publication model
- Manifest validate/diff/apply workflow (#8)
- Formalizing backend capability semantics or MCP health semantics in Lean
