# Cross-Deployment Subagent Cancel Propagation - TLA+ Spec Design

**Status:** Design
**Date:** 2026-05-13
**Tracks:** issue #188; issue #183 (formal-coverage follow-ups); sibling to #176 (cross-deployment completion projection); issue #155 (cross-boundary verification strategy)
**Scope:** TLA+ spec design for cross-deployment delivery of a cascade interrupt from a parent bridge row on deployment A to the child request owner on deployment B. The Lean bridge transition remains the source of truth for the local `bridge_cancel_cascade` state change; this model verifies delivery and receiver idempotency across the deployment boundary.

## Background

#176 modeled background subagent completion projection when the parent bridge row lives on A and the child terminalizes on B. That model intentionally records `cancelRequested[child]` when parent cancellation wins, but does not deliver the interrupt to B. The formal coverage audit calls that out as a remaining gap: cross-deployment cascade-cancel delivery is still asserted by code review rather than by a model.

This artifact closes that deferred non-goal. It verifies that once A has durable cascade intent for a running child edge, retry over the cross-deployment channel eventually causes B to durably handle the cancel under finite drops and crashes. If B handles the cancel while the child is still live, the child request terminalizes as `Interrupted`. If the child naturally terminalizes before the cancel arrives, the cancel is absorbed as an idempotent no-op against the already-terminal child.

## Brainstorming decisions

The approved first model commits to these choices.

1. **Sibling spec, not an extension of `SubagentCompletion.tla`.** Completion projection and cancel propagation are separate liveness surfaces. A sibling keeps #176 semantically unchanged, avoids growing the completion state space, and makes the cancel channel resemble #162's request-response substrate.
2. **Model delivery, not the Lean bridge transition.** `InvokeBridgeCancelCascade(child)` is the abstract A-side result of the already-modeled Lean bridge transition. It creates durable cancel intent on A. The TLA+ model starts at that boundary and proves delivery to B.
3. **Request-response cascade lane.** The cancel signal is modeled as an A-to-B RPC with an ack from B to A, using the same state categories as `ReversePairing.tla`: durable intent, volatile `inFlight`, `messages`, `pendingInbound`, bounded `Drop`, `Timeout`, and bounded `Crash`.
4. **One logical receiver effect per child.** B has a durable `cancelHandledB[child]` marker and a `cancelHandleCountB[child]` counter. Duplicate cancel deliveries may produce duplicate acks, but the child request transition or absorbed handling happens once.
5. **Natural completion race stays in scope.** B may naturally terminalize the child after A invokes cascade and before the cancel RPC is processed. Late cancel handling must preserve that natural terminal state.
6. **Detach policy remains out of scope.** This model covers cascade policy only. Detach semantics were a deferred non-goal of #176 and should be modeled separately if product semantics require it.

## Investigation findings

The relevant facts from #176 are:

- A parent bridge row can be terminalized by parent cancellation.
- The completion model writes `cancelRequested[child]` as durable A-side cascade intent but never reads it.
- Completion projection treats `Cancelled` as parent-cancel only, and late child terminals do not resurrect a cancelled parent bridge.

The relevant facts from #162 are:

- Cross-boundary models should make retry, reorder, drop, duplicate, timeout, crash, and recovery explicit.
- In-flight network state is volatile and can evaporate on crash.
- Durable state is what recovery can rely on.
- Receiver-side persist-before-ack and idempotent handlers are the key obligations for request-response correctness.

The relevant facts from the Lean bridge model are:

- `bridge_cancel_cascade` writes child interrupt intent when the parent terminalizes under cascade policy.
- `bridge_cancel_cascade` is a local bridge transition. It does not prove delivery to a remote child request owner.
- `cascade_cancels_child` and `detach_does_not_cancel_child` remain Lean-level semantics. This TLA+ artifact verifies only the cross-deployment delivery path for cascade.

## Goals

- Specify a TLA+ model whose traces represent cross-deployment cascade-interrupt delivery over lossy, reordered, duplicate-prone request-response delivery.
- Prove liveness under finite drops, finite crashes, fair retry, and fair delivery: durable A-side cascade intent for a live child eventually reaches durable B-side cancel handling.
- Prove receiver idempotency: repeated cancel deliveries and retries do not double-terminalize the child request.
- Prove the cancellation/completion race: if the child naturally terminalizes before the cancel arrives, late cancel handling is absorbed without rewrite, resurrection, or double terminal write.
- Reuse the cross-boundary primitive shape from `ReversePairing.tla` so future substrate changes can be compared across artifacts.
- Derive implementation requirements for R5 cross-deployment subagents.

## Non-goals

1. **Semantic changes to `SubagentCompletion.tla`.** This artifact may reference #176 but must not refactor or change its verified properties.
2. **Lean bridge proof changes.** The local bridge transition remains modeled in `Proofs/Subagent/Transition.lean`; this work is TLA+ only.
3. **Production code.** No Rust implementation is part of this issue.
4. **Detach policy.** Cascade only.
5. **Foreground parent progress.** This model verifies cancel delivery to B, not parent unblocking or foreground await-mode progress.
6. **Child claim/admission/execution internals.** The child request is abstracted as either `Running`, naturally terminal, or interrupted by B-side cancel handling.
7. **Arbitrary fanout.** The default model uses one child for tractable liveness. A two-child sanity config may be added if the default run stays small.
8. **Authorization, routing, and behavior-owner correctness.** The model assumes A is the parent owner and B is the child behavior owner.
9. **DefraDB/libp2p engine correctness.** As in #162 and #176, storage and transport internals sit below the model boundary.

## Model

### Topology and bounded constants

Default bounded parameters:

| Constant | Default | Meaning |
|---|---:|---|
| `Deployment` | `{A, B}` | Parent bridge owner A and child request owner B |
| `ParentDeployment` / `ChildDeployment` | `A` / `B` | Routing boundary for cancel delivery |
| `Child` | `{c1}` | One live child edge for the default liveness run |
| `RPCId` | bounded pool | Fresh ids for cancel and ack RPC attempts |
| `MaxCrashes` | small Nat | Per-deployment crash budget |
| `MaxDrops` | small Nat | Total network drop budget |
| `NoOf` | sentinel | Ack provenance sentinel for non-ack RPCs |

Child request states:

```tla
ChildState == {"Running", "Completed", "Failed", "Dead", "Superseded", "Interrupted"}
NaturalTerminal == {"Completed", "Failed", "Dead", "Superseded"}
TerminalSource == {"None", "Natural", "CascadeCancel"}
```

The default initial state has every modeled child in `Running`. `InvokeBridgeCancelCascade(child)` is enabled only while the child is live in the abstract model, matching the issue's liveness seed: "when the parent invokes `bridge_cancel_cascade` and the child has a live tool/subagent edge."

### Durable A-side state

| Field | Persistence | Meaning |
|---|---|---|
| `cancelIntentA[child]` | persisted on A | A has invoked cascade cancel for this child |
| `cancelAckedA[child]` | persisted on A | A has received B's durable-handling ack |
| `cancelAttemptCountA[child]` | history/bookkeeping | Number of cancel RPC attempts issued by A |

`InvokeBridgeCancelCascade(child)` records durable intent. It does not directly mutate B-side child state.

`EmitCancel(child)` sends a fresh cancel RPC while `cancelIntentA[child]` is true and no matching cancel RPC is currently in flight from A. After `Timeout`, crash, or dropped messages clear the volatile attempt, `EmitCancel` may retry.

### Cross-deployment channel

RPC records:

```tla
RPC == [
  id    : RPCId,
  kind  : {"Cancel", "Ack"},
  src   : Deployment,
  tgt   : Deployment,
  child : Child,
  of    : RPCId \cup {NoOf}
]
```

Volatile channel state mirrors `ReversePairing.tla`:

| Field | Persistence | Meaning |
|---|---|---|
| `inFlight[deployment]` | volatile | RPCs sent by this deployment without matching ack |
| `messages` | network | In-transit RPCs |
| `pendingInbound[deployment]` | volatile | Delivered but unprocessed RPCs |
| `rpcIdsUsed` | model bookkeeping | Fresh-id tracking |
| `dropCount` | model bookkeeping | Bounded finite drops |
| `crashCount[deployment]` | model bookkeeping | Bounded finite crashes |

Actions:

| Action | Effect |
|---|---|
| `EmitCancel(child)` | A emits a cancel RPC for durable intent, records it in `inFlight[A]`, and puts it in `messages`. |
| `Deliver(rpc)` | Moves an in-transit RPC to the target's `pendingInbound`. |
| `Drop(rpc)` | Removes an in-transit RPC while `dropCount < MaxDrops`. |
| `ProcessCancel(rpc)` | B durably handles a cancel RPC, applies `Interrupted` if the child is still running, otherwise absorbs into the existing terminal; emits an ack atomically after durable handling. |
| `ReceiveAck(ack)` | A receives B's ack, records `cancelAckedA[child]`, and clears the matching in-flight cancel. |
| `Timeout(rpc)` | A clears an in-flight cancel without changing durable state; retry can re-emit. |
| `Crash(deployment)` | Clears that deployment's `inFlight` and `pendingInbound`; durable A and B state survive. |

`Drop`, `Crash`, and `NaturalTerminalize` have no fairness. Delivery, processing, timeout, and retry have fairness where needed for liveness.

### Durable B-side state

| Field | Persistence | Meaning |
|---|---|---|
| `childStateB[child]` | persisted on B | Child request state |
| `terminalSourceB[child]` | persisted/history | `None`, `Natural`, or `CascadeCancel` |
| `terminalWriteCountB[child]` | history | Number of child terminal writes |
| `cancelHandledB[child]` | persisted on B | B has durably handled A's cancel intent |
| `cancelHandleCountB[child]` | history | Number of durable cancel-handling effects |

`ProcessCancel` is split by current child state:

- If `childStateB[child] = "Running"`, B writes `Interrupted`, sets `terminalSourceB[child] = "CascadeCancel"`, increments `terminalWriteCountB[child]`, marks `cancelHandledB[child]`, and emits an ack.
- If the child is already in `NaturalTerminal`, B marks `cancelHandledB[child]` and emits an ack without changing `childStateB` or `terminalSourceB`.
- If `cancelHandledB[child]` is already true, duplicate cancel RPCs are durable no-ops and may still emit an ack.

`NaturalTerminalize(child, terminal)` is enabled while `childStateB[child] = "Running"` and `terminal \in NaturalTerminal`. It models a child finishing before the cancel RPC arrives.

## Modeling assumptions

These are real-system obligations for the proof to apply.

- **Durable cancel intent on A.** The parent-side cascade decision must be persisted before relying on remote delivery or retry.
- **Receiver persist-before-ack.** B may emit an ack only after durably recording cancel handling. If the child is live, the interrupt terminal write and handling marker are committed before ack.
- **Idempotent B-side handler.** Duplicate cancel RPCs for the same child are no-ops after the first durable handling.
- **Retry from durable intent.** If A crashes, loses in-flight state, times out, or sees a dropped message, recovery can re-emit from `cancelIntentA`.
- **Timeout is liveness-only.** Timeout clears volatile in-flight state so retry can proceed. It must not mutate child or parent terminal state.
- **Finite instability.** Drops and crashes are finite within the checked bound; fair retry and fair delivery eventually get a post-instability attempt to B.
- **Fresh ids are a model bound.** Real RPC ids are effectively unbounded. TLC configs must leave headroom or use documented constraints to avoid bounded-pool artifacts.

## Properties

### Safety

`TypeOK`

All variables stay within their finite domains.

`RPCIdsTracked`

Every RPC in `inFlight`, `messages`, or `pendingInbound` has an id in `rpcIdsUsed`.

`RPCWellFormed`

Cancel RPCs travel from parent deployment to child deployment with `of = NoOf`. Ack RPCs travel from child deployment to parent deployment and reference a used RPC id.

`CancelIntentDurable`

Ack and B-side handling are always justified by A-side durable intent:

```tla
\A child \in Child :
  cancelHandledB[child] \/ cancelAckedA[child] => cancelIntentA[child]
```

`AckRequiresHandled`

Any ack emitted by B or recorded by A is backed by durable B-side handling:

```tla
\A child \in Child :
  cancelAckedA[child] => cancelHandledB[child]
```

`CancelHandledIdempotent`

```tla
\A child \in Child : cancelHandleCountB[child] <= 1
```

This allows duplicate inbound RPCs and duplicate acks, but only one durable B-side handling effect.

`InterruptExactlyOnce`

```tla
\A child \in Child :
  terminalWriteCountB[child] <= 1
```

Together with `CancelHandledIdempotent`, this proves repeated cancel deliveries do not double-write the child request.

`CascadeInterruptsOnlyRunning`

If cascade is the terminal source, the child is interrupted and B handled a cancel:

```tla
\A child \in Child :
  terminalSourceB[child] = "CascadeCancel" =>
    /\ childStateB[child] = "Interrupted"
    /\ cancelHandledB[child]
```

`NaturalTerminalStableAfterCancel`

If natural completion wins the race before cancel handling, late cancel handling does not rewrite the natural terminal:

```tla
\A child \in Child :
  terminalSourceB[child] = "Natural" =>
    /\ childStateB[child] \in NaturalTerminal
    /\ terminalWriteCountB[child] = 1
```

`InterruptedOnlyByCascade`

```tla
\A child \in Child :
  childStateB[child] = "Interrupted" => terminalSourceB[child] = "CascadeCancel"
```

### Liveness

`CancelDeliveryProgress`

```tla
\A child \in Child :
  cancelIntentA[child] ~> cancelHandledB[child]
```

Read: once A has durable cascade intent, fair retry/delivery eventually gets the cancel to durable B-side handling. If the child naturally terminalizes before the cancel arrives, handling still eventually occurs and is absorbed.

`CancelAckProgress`

```tla
\A child \in Child :
  cancelHandledB[child] ~> cancelAckedA[child]
```

Read: durable B-side handling eventually becomes visible to A through an ack, assuming the ack lane is fair and finite drops/crashes do not continue forever.

`LiveCancelInterruptsOrNaturalWins`

```tla
\A child \in Child :
  cancelIntentA[child] /\ childStateB[child] = "Running"
    ~> childStateB[child] = "Interrupted"
       \/ terminalSourceB[child] = "Natural"
```

Read: after A invokes cascade while the child is live, the child cannot remain running forever. Either the cancel interrupts it, or natural terminalization wins the race before the cancel is processed.

`CancelPropagationProgress`

The default TLC property combines the three leads-to claims:

```tla
CancelPropagationProgress ==
  /\ CancelDeliveryProgress
  /\ CancelAckProgress
  /\ LiveCancelInterruptsOrNaturalWins
```

## Bounded-model caveats

The model uses a bounded `RPCId` pool. If TLC reaches a state where all ids are consumed before delivery converges, a strict "every intent has an in-flight or enabled retry" invariant can fail only because the finite pool is exhausted. As in #162's `InFlightJustified`, any such support property should be documented in `.tla` comments and excluded from the default TLC enforcement unless the default bound leaves enough headroom.

The default run should enforce the main safety invariants and `CancelPropagationProgress`. A larger sanity config may raise `Child` to `{c1, c2}` or `MaxCrashes`, but the default target remains under five minutes on reference hardware.

## Derived requirements

1. **Persist cascade intent before remote delivery.** A must record the cascade intent durably before relying on any remote cancel send.
2. **Retry cancel from durable intent.** Restart, timeout, or dropped RPC must not lose the cancel. A recovery worker must be able to re-emit from persisted intent.
3. **B-side persist-before-ack.** B must not ack a cascade cancel until the child interrupt or absorbed handling marker is durable.
4. **B-side handler idempotency.** Duplicate cancel RPCs must be safe after the first durable handling.
5. **Natural terminal wins must be stable.** A late cascade cancel must not rewrite `Completed`, `Failed`, `Dead`, or `Superseded` to `Interrupted`.
6. **Timeouts are liveness-only.** A timeout can clear in-flight attempt state but cannot infer child state or mutate terminal state.
7. **Ack visibility is diagnostic, not the safety boundary.** The safety boundary is B's durable handling. A ack receipt is useful for observability and retry retirement.

## Harness

A future harness can map model actions to real operations:

| TLA+ action | Harness operation |
|---|---|
| `InvokeBridgeCancelCascade` | Cancel parent request/tool under cascade policy after spawning a cross-deployment child |
| `EmitCancel` | Projection/cancel worker sends the remote cascade RPC |
| `Drop` / delayed `Deliver` | Network partition, proxy drop, or test transport hook |
| `Crash(A)` / `Crash(B)` | Kill and restart the corresponding deployment with persisted stores retained |
| `NaturalTerminalize` | Drive the child request to a natural terminal before remote cancel is delivered |
| `ProcessCancel` | B-side remote cancel handler applies or absorbs the interrupt |

Observed state should include A-side cancel intent/ack rows and B-side child request terminal plus any cancel-handling marker. A conformance failure is either a model/implementation mismatch or a harness translation bug.

## Open questions and deferred extensions

- **Two-child default.** If one-child liveness is small, add a two-child sanity config to exercise child-parametric retry and duplicate delivery.
- **Foreground await mode.** Parent progress and blocked await semantics remain a separate liveness surface.
- **Detach policy.** Explicitly deferred.
- **Ackless implementation shape.** If R5 chooses fire-and-forget document gossip rather than request-response ack, this spec should be revised to keep `cancelHandledB` as the safety boundary and remove ack liveness.
- **B-side descendant fanout.** This model treats "child request interrupt" as the B-side receiver effect. Further propagation to nested local descendants belongs in the per-deployment Lean bridge model or a separate TLA+ fanout model.
