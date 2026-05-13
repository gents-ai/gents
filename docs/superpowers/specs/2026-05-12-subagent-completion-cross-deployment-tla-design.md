# Cross-Deployment Subagent Completion Projection - TLA+ Spec Design

**Status:** Design
**Date:** 2026-05-12
**Tracks:** issue #175 (source of truth); issue #155 (cross-boundary verification strategy); sibling to #162 (reverse-pairing TLA+); related follow-up #168 (persist-before-ack)
**Scope:** TLA+ spec design for background subagent terminal projection when the parent bridge row lives on deployment A and the child request terminalizes on deployment B. Implementation of the TLA+ source and TLC runs is tracked in a follow-on plan.

## Background

Per #175, this artifact is the next cross-boundary verification step after #162. #162 verified the subscription substrate: an operator-written desired state eventually converges to receiver-installed subscriptions across the P2P boundary. This spec verifies a first real consumer of that substrate: a parent agent on deployment A observes a background subagent's terminal state after the child ran on deployment B.

The in-process bridge semantics are already modeled in Lean through the R2 bridge transitions landed in #154:

- `bridge_complete` advances a running, committed parent bridge tool to `.completed` when the child request is `.completed`.
- `bridge_failure` advances a running parent bridge tool to a non-success terminal when the child request terminal is `.failed`, `.dead`, `.interrupted`, or `.superseded`. For this cross-deployment TLA+ model, every child non-completed terminal projects to parent bridge `Failed`; parent bridge `Cancelled` is reserved for `CancelParent`.
- `bridge_cancel_cascade` sets the child request's interrupt flag when the parent terminalizes under cascade policy.

This TLA+ model does not re-prove those in-process transitions. It verifies the cross-deployment preconditions and side effects around them: DefraDB document delivery, crash/recovery, duplicate observations, durable notification append, coalesced wake-up enqueue, and cancellation drain behavior.

The R4 implementation worktree is intentionally not an input to this spec. The model uses only the R4-relevant summary from the task framing as compatibility context: parent/child linkage fields, background completion projection shape, same-session queue hints, hard-coded cascade cancel policy, and depth ceiling. R4 may land before or after this artifact without changing the TLA+ source.

## Brainstorming decisions

The first tractable model commits to the following scope.

1. **Background mode only.** The model covers background child completion projection because that is where transcript notification and coalesced wake-up behavior matters. Foreground cross-deployment blocking is deferred.
2. **Two child bridges for one parent session.** One child is too weak to exercise coalescing. Two children are enough to verify that multiple background completions collapse to one pending wake-up for the same `(session_id, queue_key)`.
3. **Separate document-gossip channel.** The model treats completion delivery as DefraDB document gossip, not as the reverse-pairing admin RPC lane from #162. The #162 subscription substrate is an assumption that enables fair document delivery.
4. **No child-claim modeling.** Claiming/executing the child on B is abstracted to a B-local durable terminal write. A only learns through replicated documents.
5. **Both A and B can crash.** A crashes clear in-memory delivery/projection work but preserve bridge rows, durable local observations, transcript notifications, and queue rows. B crashes preserve durable child terminals and clear only volatile, non-durable state.
6. **Cancellation wins.** If parent cancellation terminalizes the bridge before A projects the child terminal, late child observations are absorbed as no-ops for the bridge. This refines #175's liveness claim: every child terminal eventually projects while the bridge remains live; otherwise it settles to the deterministic cancelled state.

## Investigation findings

The relevant facts from #175 are:

- The parent bridge row is written on A at spawn time.
- The child request and child response are materialized on B.
- Linkage is by parent `AgentRequest.request_id`, parent `AgentToolCall.tool_call_id`, child `AgentRequest.caused_by_parent_request_id`, child `AgentRequest.caused_by_parent_tool_call_id`, and parent `AgentToolCall.child_request_id`.
- Each `(did, behavior_id)` pair is routed to exactly one deployment, so the modeled child behavior has a single owner B.
- A learns the child terminal through DefraDB P2P delivery, not an in-process tokio event.

The relevant facts from #162 and #168 are:

- Cross-node verification should make retry, reorder, drop, duplicate, crash, and recovery behavior explicit.
- In-flight network work may evaporate on crash; durable intent/state is what recovery can rely on.
- Persist-before-ack was derived in #162 because an ack before persistence lets a receiver crash in a state the sender incorrectly treats as installed. The structurally analogous obligation here is: A must project only from a durable child terminal observation, B must persist the final response before or atomically with the child terminal, and B must not expose a terminal document before both documents are durably committed.

The relevant facts from the Lean bridge files are:

- `bridge_complete` requires a running committed parent bridge tool and a completed child.
- `bridge_failure` requires a running parent bridge tool and a non-completed terminal child.
- Both bridge projection transitions preserve the bridge identity fields and only advance the parent tool state.
- `bridge_cancel_cascade` is structurally inert except for the child interrupt flag; in this TLA+ model, parent cancellation is abstracted as the A-side bridge reaching a cancelled terminal and issuing a cascade request.

## Goals

- Specify a TLA+ model whose traces represent cross-deployment background subagent completion projection over lossy, reordered, duplicate document delivery.
- Prove bridge terminal uniqueness: a parent bridge row reaches at most one terminal and never observes both complete and failure projection.
- Prove projection durability: A never projects a terminal that B has not durably persisted, and A never projects from a volatile local delivery event that would disappear after an A crash.
- Prove duplicate observation and retry idempotency for bridge projection, durable notification append, and coalesced wake-up enqueue.
- Prove same-session queue safety: automated cancellation drain never terminalizes user-originated pending requests, and coalescing never creates two pending wake-ups with the same `(session_id, queue_key)`.
- Prove liveness under finite crashes, finite drops, and fair document delivery: a durable B terminal eventually either projects on A or is absorbed by prior parent cancellation; every projection eventually has a durable notification and a represented wake-up.
- Derive implementation obligations for R5 and any R4 queue/projection code that shares the same helper paths.

## Non-goals

1. **R4 implementation.** This artifact must not read or depend on the sibling R4 worktree.
2. **In-process happy path.** The Lean bridge proofs cover in-process transition effects. This TLA+ model verifies the cross-deployment failure modes around those transitions.
3. **Foreground await mode.** Deferred to a follow-up model because blocked-parent progress introduces a different liveness surface.
4. **Detach policy.** V1 cancel policy is cascade; detach remains out of scope.
5. **Child claim/admission/execution.** B-local execution is abstracted to a durable terminal write.
6. **Token/cost budget propagation, LLM provider behavior, host sandbox behavior.** Explicitly out of scope per #175.
7. **Multi-parent/fork semantics.** The model has one parent session and two child bridge rows.
8. **DefraDB storage-engine and libp2p correctness.** As in #155 and #162, storage and transport internals are assumed below the model boundary.
9. **Authorization and behavior-routing correctness.** The model assumes the routing map is already correct: parent behavior on A, child behavior on B.

## Model

### Topology and bounded constants

Default bounded parameters:

| Constant | Default | Meaning |
|---|---:|---|
| `Deployment` | `{a, b}` | Parent on `a`, child behavior owner on `b` |
| `Child` | `{c1, c2}` | Two background children for one parent session |
| `Session` | `{sParent}` | One same-session queue scope |
| `TerminalKind` | `{Completed, Failed, Dead, Interrupted, Superseded}` | Child request terminals |
| `EventId` | bounded pool | Fresh ids for document-gossip observations |
| `QueueId` | bounded pool | Fresh ids for pending queue rows |
| `MaxCrashes` | small Nat | Per-deployment crash budget |
| `MaxDrops` | small Nat | Total document-gossip drop budget for liveness checking |

The model may add a bigger sanity `.cfg` later with `Deployment = {a, b, c}` to verify that a third uninvolved deployment does not affect A/B completion projection. The default model should stay two-deployment for tractability.

### Durable state on B

| Field | Persistence | Meaning |
|---|---|---|
| `childDurable[child]` | persisted on B | `NoTerminal` or one terminal in `TerminalKind` |
| `childFinalResponseDurable[child]` | persisted on B | Boolean; true when the terminal response content is committed |

`PersistChildTerminal(child, terminal)` writes both the child terminal and final response durably. The model treats final-response content abstractly; only the durable-before-observable ordering matters.

### Delivery state

| Field | Persistence | Meaning |
|---|---|---|
| `messages` | network | Set of document-gossip observations in flight from B to A |
| `pendingInboundA` | volatile on A | Delivered but not yet durably recorded observations |
| `observedDurableA[child]` | persisted on A | A's local durable replica of the child terminal observation |
| `eventIdsUsed` | model bookkeeping | Freshness for bounded event ids |
| `dropCount` | model bookkeeping | Bound for finite document drops |

Document-gossip observations are records:

```tla
Observation == [
  id       : EventId,
  child    : Child,
  terminal : TerminalKind
]
```

Delivery may reorder, duplicate, and drop. Duplicates are modeled by repeated `EmitTerminalObservation` actions with fresh `EventId`s for the same `(child, terminal)` once B has durable terminal state.

The key cross-boundary persistence point is split into two actions:

1. `DeliverObservation(obs)` moves an in-flight observation into volatile A memory.
2. `PersistObservationOnA(obs)` commits that observation into `observedDurableA`.

Projection is allowed only from `observedDurableA`, not directly from `pendingInboundA`.

### Durable state on A

| Field | Persistence | Meaning |
|---|---|---|
| `bridgeState[child]` | persisted on A | `Running`, `Completed`, `Failed`, or `Cancelled` |
| `terminalSource[child]` | persisted/history | `None`, `ChildProjection`, or `ParentCancel` |
| `terminalWriteCount[child]` | history | Number of A-side terminal writes to the bridge row |
| `notificationDurable[child]` | persisted on A | Whether the `<subagent-notification>` transcript message is appended |
| `queueRows` | persisted on A | Same-session pending queue rows |
| `queueIdsUsed` | model bookkeeping | Freshness for bounded queue ids |
| `cancelRequested[child]` | persisted/observable intent | Parent cancellation cascade intent for that child |

`cancelRequested[child]` is written by `CancelParent` but is not read by the completion-projection model. It is kept as a reserved hook for the deferred cross-deployment cancel-propagation extension.

Queue rows are records:

```tla
QueueRow == [
  id      : QueueId,
  session : Session,
  source  : {"user", "subagent_completion"},
  policy  : {"append", "coalesce"},
  key     : QueueKey,
  state   : {"pending", "drained"}
]
```

The automated wake-up key is fixed:

```tla
WakeupKey(s) == "subagent_completion:" \o s
```

Two children share the same parent session and therefore the same coalescing key.

### Actions

| Action | Effect |
|---|---|
| `PersistChildTerminal(child, terminal)` | B durably writes the child terminal and final response. Enabled only if the child has no prior durable terminal. |
| `EmitTerminalObservation(child)` | If `childDurable[child] # NoTerminal`, enqueue a fresh document-gossip observation into `messages`. May fire repeatedly to model retries/duplicates. |
| `DeliverObservation(obs)` | Move `obs` from `messages` to `pendingInboundA`. |
| `DropObservation(obs)` | Remove `obs` from `messages` while `dropCount < MaxDrops`. |
| `PersistObservationOnA(obs)` | Remove `obs` from `pendingInboundA`; durably set `observedDurableA[obs.child] = obs.terminal`. Idempotent if already set to the same terminal. |
| `ProjectTerminal(child)` | If `bridgeState[child] = Running` and `observedDurableA[child] # NoTerminal`, invoke the abstract Lean bridge projection: `Completed` maps to `Completed`; every non-completed terminal (`Failed`, `Dead`, `Interrupted`, `Superseded`) maps to `Failed`. Set `terminalSource = ChildProjection` and increment `terminalWriteCount`. |
| `AppendNotification(child)` | If the bridge terminal came from child projection, durably append the `<subagent-notification>` message. Idempotent. |
| `EnqueueWakeup(child)` | If notification is durable, ensure a pending `subagent_completion` row exists for `(sParent, WakeupKey(sParent))`. If one already exists, no-op; otherwise insert one fresh row. |
| `EnqueueUserRequest` | Add a user-originated pending row. Used to make cancellation-drain safety non-vacuous. |
| `CancelParent(child)` | If the bridge is still running, terminalize it as `Cancelled`, set `terminalSource = ParentCancel`, increment `terminalWriteCount`, and set `cancelRequested[child]`. |
| `CancelDrain` | Drain pending automated wake-ups for the parent session; preserve all user-originated pending rows. |
| `CrashA` | Clear A volatile delivery/projection state (`pendingInboundA`) and increment `crashCount[a]`; preserve A durable bridge, observation, notification, queue, and cancel state. |
| `CrashB` | Clear any modeled B volatile state and increment `crashCount[b]`; preserve `childDurable` and durable final response state. |

The model does not include a separate `Recover` action. After crash, the same durable-state actions are enabled again.

### Cancellation and late completion

`CancelParent(child)` is terminalizing for the A-side bridge row. After it fires, `ProjectTerminal(child)` is disabled because the bridge is no longer `Running`. Later B terminals may still be persisted, delivered, and durably observed on A, but they do not change the bridge state, do not append a subagent notification for that bridge, and do not enqueue a completion wake-up.

This is the deterministic cancel-then-completion rule for the model:

```tla
terminalSource[child] = ParentCancel
  => bridgeState[child] = Cancelled
     /\ terminalWriteCount[child] = 1
     /\ notificationDurable[child] = FALSE
```

If product semantics later decide that a cancelled parent should still receive a transcript notification for a late child terminal, that is a deliberate model change and should be reviewed separately.

## Modeling assumptions

These are obligations on the real system for the proof to apply.

- **Projection reads durable local observations.** A's projection worker must run from durable local child terminal/response documents, not directly from volatile subscription callbacks.
- **B persists final response before terminal observability.** B must commit the final response before or atomically with the child terminal, and both documents must be durable before any document-gossip notification can cause A to observe that terminal.
- **Bridge projection is idempotent.** Duplicate observations must find the bridge already terminal and no-op, not call `bridge_complete` or `bridge_failure` twice.
- **Notification append is idempotent per bridge.** Duplicate observations and projection retries must not append multiple `<subagent-notification>` messages for the same child bridge.
- **Wake-up enqueue is coalesced atomically.** Inserting a `subagent_completion` wake-up must be conditional on no pending row already existing for `(session_id, queue_key)`.
- **Cancellation drain filters by source.** Drain removes or marks automated `subagent_completion` wake-ups only. It does not terminalize user-originated pending requests.
- **Fair document delivery after finite instability.** Drops and crashes are finite within the bounded model. Once B has durable terminal state and ids remain available, observations can be emitted and delivered fairly.
- **Fresh id pools are modeling bounds only.** Real event ids and queue ids are effectively unbounded. TLC configs must leave enough headroom to avoid bounded-pool artifacts.

## Properties

### Safety: structural and durability invariants

`TypeOK`

All state variables stay within their declared finite domains.

`BridgeTerminalUnique`

```tla
\A child \in Child :
  terminalWriteCount[child] <= 1
```

Together with bridge-state typing, this proves every bridge reaches at most one terminal: not complete and failed, not failed and cancelled, and never twice.

`ProjectionRequiresBDurableTerminal`

```tla
\A child \in Child :
  terminalSource[child] = ChildProjection
    => childDurable[child] = observedDurableA[child]
       /\ childFinalResponseDurable[child]
```

A cannot project a terminal that B has not durably persisted.

`ProjectionRequiresADurableObservation`

```tla
\A child \in Child :
  terminalSource[child] = ChildProjection
    => observedDurableA[child] # NoTerminal
```

A cannot project from a volatile delivery event that would be lost on A crash.

`ProjectionMatchesLeanBridgeMapping`

If `observedDurableA[child] = Completed`, child projection moves the bridge to `Completed`. If the observed durable terminal is `Failed`, `Dead`, `Interrupted`, or `Superseded`, child projection moves the bridge to `Failed`. `Cancelled` is reachable only via `CancelParent`.

`NotificationIdempotent`

At most one durable notification exists per child bridge, and notifications only exist after child projection.

```tla
\A child \in Child :
  notificationDurable[child] => terminalSource[child] = ChildProjection
```

`WakeupCoalesced`

There are never two pending automated wake-up rows for the same `(session_id, queue_key)`.

```tla
\A r1, r2 \in queueRows :
  r1 # r2
  /\ r1.source = "subagent_completion"
  /\ r2.source = "subagent_completion"
  /\ r1.state = "pending"
  /\ r2.state = "pending"
  /\ r1.session = r2.session
  /\ r1.key = r2.key
  => FALSE
```

`WakeupCausal`

Any pending automated wake-up for the parent session is justified by at least one durable child notification.

`CancelDrainPreservesUserPending`

User-originated pending queue rows remain pending across cancellation drain. In the default model, this is made non-vacuous by enabling `EnqueueUserRequest`.

`ParentCancelAbsorbsLateTerminal`

If parent cancellation terminalized the bridge first, later child terminal observation cannot change the bridge state, cannot create a child-projection source, and cannot enqueue a completion notification.

### Liveness: fair-delivery progress

The liveness properties use disjunctive forms, following the #162 convergence shape, because cancellation is an external action that can legitimately invalidate the stricter "must project" obligation.

`DurableTerminalSettles`

```tla
\A child \in Child :
  childDurable[child] # NoTerminal
  /\ terminalSource[child] = None
    ~> terminalSource[child] = ChildProjection
       \/ terminalSource[child] = ParentCancel
```

Read: once B has durably terminalized a child, A eventually either projects that terminal or has already cancelled the bridge.

`LiveBridgeTerminalProjects`

```tla
\A child \in Child :
  childDurable[child] # NoTerminal
  /\ bridgeState[child] = Running
  /\ terminalSource[child] = None
    ~> terminalSource[child] = ChildProjection
       \/ terminalSource[child] = ParentCancel
```

Read: while the bridge remains live, the terminal cannot stay invisible forever under fair delivery.

`ProjectionNotifies`

```tla
\A child \in Child :
  terminalSource[child] = ChildProjection
    ~> notificationDurable[child]
```

`ProjectionWakeupRepresented`

```tla
\A child \in Child :
  notificationDurable[child]
    ~> \E row \in queueRows :
         row.source = "subagent_completion"
         /\ row.policy = "coalesce"
         /\ row.session = sParent
         /\ row.key = WakeupKey(sParent)
         /\ row.state = "pending"
```

Read: every projection eventually produces a wake-up representation. If another child already created the pending coalesced wake-up, the property is already satisfied for the later projection.

`CancelThenCompletionSettles`

If `CancelParent(child)` wins before child projection, all future states keep the bridge cancelled even if B later persists and A later durably observes the child terminal.

This is mostly a stability/safety property, but the liveness check should include traces where cancellation precedes `PersistChildTerminal` so TLC exercises the late-arrival interleaving.

## Boundary discipline: timeouts

The model has no timeout that makes a safety decision. Dropping a document-gossip observation only removes an in-flight message while `dropCount < MaxDrops`; it does not mutate bridge state, notification state, or queue state. Progress comes from durable B state plus re-emission under fairness, not from timeout-based inference.

If a future implementation wants to infer child failure from "no terminal observed after T", that is outside this model and violates #155's timeout discipline unless separately modeled as a liveness-only mechanism.

## Derived requirements

These requirements are expected outputs of the model. They should be named in the eventual PR so they can feed R5 and any shared R4 helpers.

1. **Persist-before-projection on A.** A's projection worker must consume durable local child terminal/response rows, not volatile subscription events. Otherwise A can project, crash, lose the observation, and come back with a parent bridge terminal it cannot justify.
2. **Persist-before-observe on B.** B must commit the child terminal and final response before emitting or making observable the document event that A can project from. This is the subagent-completion sibling of #168.
3. **Final response before terminal observability.** Because the real system stores terminal request state and final response as separate DefraDB documents, B must persist the final response before or atomically with the child terminal. A terminal document must not become observable on A before the corresponding final response document is durable and replicable.
4. **Atomic coalesced wake-up insert.** The queue layer needs a transaction, unique pending index, or conditional upsert for `(session_id, queue_key, state = pending)` automated wake-ups. A read-then-insert race can violate `WakeupCoalesced`.
5. **Projection-side idempotency.** Duplicate child terminal observations, repeated document catch-up, and projection retries must be harmless after the bridge is terminal. The handler should explicitly check bridge terminal state before invoking `bridge_complete` or `bridge_failure`.
6. **Notification-before-wake-up.** The transcript notification must be durable before the wake-up request is enqueued. Otherwise the parent can wake and not find the durable `<subagent-notification>` it woke for.
7. **Cancellation drain source filter.** The drain operation must filter on `metadata.queue.source = subagent_completion` and the coalesce key. User-originated pending work must survive.
8. **Late child terminal after cancellation is a no-op for the parent bridge.** This deterministic rule should be an explicit implementation contract so retries/duplicates cannot resurrect a cancelled bridge.

The implementation plan should include an optional "unsafe early projection" TLC configuration or temporary mutant action to demonstrate the counterexample behind requirements 1 and 2, then keep the safe transition relation in the checked model.

## Harness

The eventual multi-node harness mirrors the #162 shape, but the scenario vocabulary is document/projection oriented rather than admin-RPC oriented.

1. **Process orchestration.** Spawn two agents/deployments with routing such that the parent behavior lives on A and child behavior lives on B.
2. **Driving channel.**
   - `PersistChildTerminal` maps to driving or faking a B-side child request terminal write in a controlled test.
   - `CrashA` / `CrashB` map to process kill/restart with persisted stores retained.
   - `DropObservation` / delayed `DeliverObservation` map to network partition/proxy delay or a test DefraDB replication harness.
   - `CancelParent` maps to parent request cancellation or bridge cancellation through the agent-facing API once available.
3. **Observation channel.** Query/subscribe to A-side `AgentToolCall`, `AgentMessage`, and `AgentRequest` queue rows plus B-side child `AgentRequest` / response rows.
4. **Conformance check.** At every observed state, evaluate bridge uniqueness, durable projection justification, queue coalescing, and user-pending preservation. After quiescence, evaluate the liveness target for terminal projection or deterministic cancellation.

Harness work is not part of this TLA+ artifact, but the model should keep action names and state labels close enough to make future trace-to-scenario export straightforward.

## Open questions and deferred extensions

- **Foreground mode.** Foreground cross-deployment subagents block parent progress while delivery is remote and failure-prone. That needs a separate liveness model with parent progress and timeout discipline in scope.
- **Three-deployment sanity.** The default model is A/B only. A later `.cfg` should add an uninvolved deployment C to verify routing assumptions do not accidentally depend on `Deployment = {a, b}`.
- **Crash-enabled two-child state space.** Two children are required for coalescing coverage. If liveness with two children and crashes is too large, follow #167's pattern: ship a tractable default plus a documented bigger sanity config.
- **Per-terminal failure coverage.** If all four non-completed terminal kinds make the default state space too large, use a default terminal abstraction `{Completed, Failed}` and a second safety-only config with `{Failed, Dead, Interrupted, Superseded}`. The README must call out the limitation.
- **Queue ordering.** The model checks preservation and coalescing, not created-at ordering among user requests. Same-session FIFO ordering is a separate queue model.
- **Child cancellation delivery.** `CancelParent` records cascade intent but does not model the cross-deployment delivery of that interrupt to B. This artifact is about completion projection, not cancellation propagation.

## Implementation steps (informational; not part of this spec)

1. Translate this model into raw TLA+ in `crates/defra-agent/proofs/tla/SubagentCompletion.tla`.
2. Add `MCSubagentCompletion.tla` and a default `.cfg` with two children, two deployments, bounded crashes, bounded drops, and enough event/queue ids for liveness.
3. Add safety invariants first and run TLC before enabling liveness.
4. Add fairness annotations and leads-to properties.
5. Add an optional unsafe early-projection mutant/config to capture the persist-before-projection counterexample as a derived requirement.
6. Update `crates/defra-agent/proofs/tla/README.md` with run commands, expected state counts, limitations, and any excluded properties.
7. Open the PR with `Closes #175`, cite #162, #155, and #168, and list derived follow-up requirements.
