# Runtime Reconciliation

Date: 2026-04-09
Depends on: sourcenetwork/defra-agent#9 (principal/behavior split), tool system design spec

## Problem

The runtime resolves all behavior configuration once at startup. `resolve_tool_surfaces()` runs once, each `BehaviorDaemon` builds its rig `Agent` once, and the scheduler captures tool surfaces at construction. Changing an AgentBehavior's prompt, swapping a ToolSelection, or updating an InferenceProfile requires a full process restart.

The runtime is document-driven but not document-reactive. The control plane documents define desired state, but the runtime only reads them once. This makes operational iteration slow and prevents the system from converging on desired state without manual restarts.

## Design Decisions

- **Fully reactive, event-bus driven.** No polling loops, no trigger documents, no explicit reconcile requests. The DefraDB event bus already delivers update notifications. The control watcher subscribes to updates on the collections that define behavior configuration and reacts to changes as they arrive.
- **Debounce coalesces rapid updates.** A 5-second quiet period after the last relevant event before reconcile fires. This handles multi-document updates (e.g., changing an AgentBehavior to point to a new ToolSelection while also updating the InferenceProfile) arriving in quick succession, including with P2P gossip jitter.
- **Full re-resolve, not incremental tracking.** The reconcile cycle does not track which specific documents changed. It re-resolves the complete behavior set from DB and diffs against the current generation. The cost is trivial (a handful of GraphQL queries every 5+ seconds at most) and the simplicity is worth it.
- **Atomic snapshots via watch channel.** One `RuntimeSnapshot` published through a `watch::Sender`. All consumers (router, supervisor, scheduler) see the same generation. No partial updates, no timing windows where one component knows about a change and another doesn't.
- **In-flight requests finish on their generation.** A reconcile does not interrupt running requests. New requests pick up the latest generation. Two generations may briefly coexist. This is safe because the old generation was valid when the request started.
- **Full behavior set reconciliation.** Adding behaviors, removing/disabling behaviors, updating behavior configs, and changing the principal's `default_behavior_id` are all reconcile scope.
- **AgentPrincipal identity is immutable.** The `agent_did` never changes. Only `default_behavior_id` (and potentially `display_name`) can change on the principal document.
- **No new schemas or collections.** The control watcher is purely internal. Observability is through tracing, not audit documents.
- **No Lean spec changes required.** Request lifecycle, session semantics, and state machine transitions are untouched. The reconcile state machine is internal runtime machinery, not a formal behavioral property.

## Reconcile State Machine

```
Idle -> Debouncing -> Resolving -> Diffing -> Applying -> Idle
```

**Idle:** Waiting for event bus updates on watched collections (AgentBehavior, ToolSelection, InferenceProfile, AgentPrincipal). Zero CPU cost -- blocked on the event bus subscription.

**Debouncing:** A relevant event was received. A 5-second timer is running. Each new relevant event resets the timer. No DB queries during this phase.

**Resolving:** The debounce timer expired. The watcher queries DefraDB for the full behavior set:
1. Query `AgentPrincipal` for `default_behavior_id`
2. Query all `AgentBehavior` documents for this principal's `agent_did`
3. For each behavior, resolve its `ToolSelection`, `InferenceProfile`, and `InferenceBackend`
4. Partition into runnable vs unrunnable (same rules as startup -- missing or unhealthy backend = unrunnable)
5. Build `BehaviorConfig` + `ToolSurface` per runnable behavior

**Diffing:** Compare the resolved snapshot against the current generation using structural equality. If identical, transition to Idle (no-op). Log at `debug` level.

**Applying:** The snapshot differs. Increment generation counter, publish new `RuntimeSnapshot` via `watch::Sender`. Log at `info` level with behavior add/remove/update counts. Transition to Idle.

**Error handling:** Errors during Resolving or Applying are logged at `error` level and the machine returns to Idle. The next document change will trigger another cycle. Reconcile failures are transient -- they never poison the running generation.

## RuntimeSnapshot

```rust
struct RuntimeSnapshot {
    generation: u64,
    default_behavior_id: String,
    behaviors: HashMap<String, Arc<BehaviorConfig>>,
    tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    unavailable_behaviors: HashMap<String, String>,
}
```

Generation is a monotonic counter starting at 1 (the initial startup resolution). The startup path produces the first snapshot using the same resolution logic. Reconcile and startup share one code path for resolution.

The snapshot is published through `watch::Sender<Arc<RuntimeSnapshot>>`. Consumers hold `watch::Receiver<Arc<RuntimeSnapshot>>` and check for new generations at their natural decision points.

## Control Watcher

A new `ControlWatcher` is spawned in `run_agent()` alongside the router and supervisor. It:

1. Subscribes to the DefraDB event bus for update events (same `node.subscribe(&[EventName::Update])` mechanism as `DefraWatcher`)
2. Filters by `collection_id` on the `Update` event to match AgentBehavior, ToolSelection, InferenceProfile, and AgentPrincipal collections. Unlike `DefraWatcher` which only processes P2P-relayed events (`is_relay`), the control watcher reacts to both local and P2P updates since configuration changes may arrive via either path
3. On a relevant event, resets the 5-second debounce timer
4. On debounce expiry, runs the resolve/diff/apply cycle
5. Publishes new snapshots when configuration has changed

The watcher is cancellation-aware via the existing `CancellationToken` and shuts down cleanly with the rest of the runtime.

## Consumer Contracts

### Router

The router holds a `watch::Receiver<Arc<RuntimeSnapshot>>`. At the top of its dispatch loop (before processing the next request from the watcher), it checks `has_changed()`. If a new snapshot is available, it rebuilds its dispatch map (`behavior_id -> mpsc::Sender<AgentRequest>`) and its `default_behavior_id` from the new snapshot.

Requests for behaviors that no longer exist get the same "behavior unavailable" error response path that already exists for unrunnable behaviors.

### Supervisor

The supervisor consumes the same snapshot. When a new generation arrives, it performs set arithmetic against the running executor set:

**Behavior added** (in new snapshot, not running): Spawn a new executor. Create a new request channel. The router's rebuilt dispatch map will start sending requests to it.

**Behavior removed** (running, not in new snapshot): Drop the `mpsc::Sender` for that behavior. The executor's receiver yields `None`, causing it to exit cleanly. In-flight requests finish. The supervisor reaps the task normally.

**Behavior updated** (in both, config or tool surface differs): Treat as remove-old + add-new. Drop the old sender, spawn a fresh executor with the new config. Requests queue in the new channel during the brief transition. In-flight requests on the old executor finish on their generation.

### Scheduler

The scheduler holds a `watch::Receiver<Arc<RuntimeSnapshot>>`. At each tick cycle (60 seconds), before scanning for due tasks, it checks `has_changed()`. If a new snapshot is available, it swaps its local behavior and tool surface references.

The scheduler already builds a fresh rig `Agent` per task execution, so it naturally picks up new configs on the next run. A running task finishes with whatever it started with. If a scheduled task references a `behavior_id` absent from the snapshot, it records `last_error` as it does today for unrunnable behaviors.

## Startup Convergence

The initial startup resolution is refactored to produce the first `RuntimeSnapshot` (generation 1) using the same resolution code path that reconcile uses. This eliminates duplicate resolution logic and guarantees that startup and reconcile produce structurally identical snapshots.

The startup sequence becomes:
1. Boot DefraDB, establish principal
2. Resolve initial `RuntimeSnapshot` (generation 1)
3. Publish snapshot via watch channel
4. Spawn control watcher, router, supervisor, scheduler -- all consuming the watch channel
5. Control watcher enters Idle, waiting for document changes

## Out of Scope

- MCP service-registry-triggered rebuilds (transient health changes handled by error returns at call time)
- Implicit reconcile on every document write without debounce
- Reconcile audit documents or status collections
- Changes to request lifecycle, session semantics, or formal state machines
- Manifest validate/diff/apply workflow (#8)
- Lean spec updates (no behavioral property changes)
