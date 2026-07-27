# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `ReversePairing` — control-plane convergence of subscription/replicator reverse-pairing between two peers. Spec design: `docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md` (removed from the tree; see git history). Implementation plan: `docs/superpowers/plans/2026-05-08-reverse-pairing-tla-spec.md` (removed from the tree; see git history).
- `SubagentCompletion` — background subagent terminal projection where the parent bridge row lives on deployment A and the child terminalizes on deployment B. Spec design: `docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md` (removed from the tree; see git history). Implementation plan: `docs/superpowers/plans/2026-05-12-subagent-completion-cross-deployment-tla-spec.md` (removed from the tree; see git history).
- `SubagentCancelPropagation` - cascade-cancel delivery from a parent bridge row on deployment A to the child request owner on deployment B. Spec design: `docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-tla-design.md` (removed from the tree; see git history). Implementation plan: `docs/superpowers/plans/2026-05-13-subagent-cancel-propagation-tla.md` (removed from the tree; see git history).
- `PairingTransport` — connection establishment + replication liveness for one directed pairing edge, the transport layer *below* `ReversePairing`. `ReversePairing` and the Lean `PairingReconcile` model both assume the transport carries the install RPC / that `connect` succeeds; neither models *establishing* the link, so the live #511 fleet hang was outside the modeled world. This spec closes that gap. It distinguishes the **three real failure modes** (grounded in the production code) with two independent BOOLEAN constants — `Dialable` (the connect-gate ticket form) and `ReplicatorInstallable` (whether `add_replicator` can succeed once connected) — exercised by a **three-config diagnostic**: `MCPairingTransportDialable` (both true: the shareable-address fix — all properties hold); `MCPairingTransportUndialable` (MODE A — `Dialable = FALSE`: connect-fails-first, the *literal* #511 hang, never reaches `Connected` so nothing subscribes and no applied row is written); `MCPairingTransportReplicatorStuck` (MODE B/C — `ReplicatorInstallable = FALSE`: connect OK and collections subscribe but the replicator install never succeeds — the durable "subscribed collections, `replicator_addresses` null" partial row). Companion to the Lean `PairingReconcile` `dialFailed` (MODE A) and `reconcileInstallReplicatorFailed` (MODE B/C) transitions and the `convergence_requires_successful_install` obligation.
- `P2PBackpressure` — hub fan-in/fan-out admission **obligation** model for #630, not a multi-wave flood-safety proof. `PairingTransport` proves a single edge can connect/install; this model starts after that. Its one-wave necessity obligations say outbound timeouts release push-worker slots and inbound success acks are backed by modeled pending registration or merge. The pinned DefraDB implementation has since added bounded queue admission before worker execution, per-peer scheduling/coalescing, persisted retry handoff on overflow, and durable push-originated pending-DAG recovery. Those multi-wave and restart properties, plus Bitswap stalls, rate limits, gossip-loop health, multi-slow-peer fill, and peer-vs-CID pending, remain outside this model. See `boundary.p2p-backpressure.obligation-model`. Green/red configs (`MCP2PBackpressureGreen` / `TimeoutStall` / `BadAck`) are **modeled, not yet TLC-checked**.
- `ReplicatedRequestConvergence` — owner-only terminal convergence for
  party-scoped replicated `AgentRequest` documents (#664/#661/#683). Replica
  holders are authorized request parties, never unrelated fleet pairings. A
  persisted per-request `Cap` bounds blind same-value writes across owner
  restarts; each write fans out only to online party replicators. A party peer
  unavailable through the entire cap converges when
  pairing recovery reinstalls the configured replicator and performs one full
  replay of current owner-authored DAG heads. Replay authors no request field,
  so partitions do not create unbounded CRDT history. `SingleClaimer` fences
  passive foreign replicas; `TerminalConverges` depends on fair peer recovery
  and replay. The `Stuck` diagnostic disables replay to prove that recovery is
  load-bearing; `PeerClaim` arms an illegal foreign claim to prove the safety
  invariant is falsifiable. Rust bindings cover persistent cap/window behavior,
  real P2P replay after cap exhaustion, and reconnect-triggered replicator
  reinstall. Spec design: `docs/superpowers/specs/2026-07-08-replicated-request-convergence-664-design.md` (removed from the tree; see git history).
- `Sanity` — toolchain smoke test; not a real model.

## One-time setup

```bash
./scripts/install-tools.sh
```

Downloads `tla2tools.jar` into `.tools/` (gitignored). Requires Java 11+ on `PATH`. On macOS without a JDK, install via `brew install openjdk@17` and ensure `/opt/homebrew/opt/openjdk@17/bin` is on `PATH`. Override version via `TLA_VERSION=v1.8.0`.

## Running

For Sanity (toolchain smoke test):
```bash
./scripts/run-tlc.sh Sanity
```

For ReversePairing (the real model):
```bash
./scripts/run-tlc.sh MCReversePairing
```

For the multi-collection ReversePairing sanity bound:
```bash
./scripts/run-tlc.sh MCReversePairingMulti
```

For SubagentCompletion:
```bash
./scripts/run-tlc.sh MCSubagentCompletion
```

For SubagentCancelPropagation:
```bash
./scripts/run-tlc.sh MCSubagentCancelPropagation
```

For PairingTransport — the dialable model checks clean (all properties hold):
```bash
./scripts/run-tlc.sh MCPairingTransportDialable
```

The other two configs are diagnostics and are EXPECTED to report violations — each reproduces a distinct real failure mode. MODE A (connect-fails-first, the literal #511 hang) violates `ReplicatorLiveness` with a never-`Connected` trace:
```bash
./scripts/run-tlc.sh MCPairingTransportUndialable
```

MODE B/C (connect OK, replicator install never succeeds) violates `PartialApplyHasProgress` and returns the exact "subscribed collections, null replicator" partial row:
```bash
./scripts/run-tlc.sh MCPairingTransportReplicatorStuck
```

For P2PBackpressure — **modeled, not yet TLC-checked** on the reference
runner (no recorded-runs row; TLC was not executed when this model landed).
The configs below are the intended green/red suite; treat the prose outcomes
as design claims until real TLC rows are pasted into "Recorded runs":
```bash
./scripts/run-tlc.sh MCP2PBackpressureGreen
```

The other two configs are diagnostics and are **designed** to report
violations (unverified until TLC is run). `TimeoutStall` is intended to
violate `HealthyPeersDeliver` by filling the only push slot with a
nonresponsive peer that never releases it:
```bash
./scripts/run-tlc.sh MCP2PBackpressureTimeoutStall
```

`BadAck` is intended to violate `SuccessAckBacked` by success-acking at
pending-DAG capacity without registering or merging the DAG:
```bash
./scripts/run-tlc.sh MCP2PBackpressureBadAck
```

For ReplicatedRequestConvergence — the green model checks clean (both properties hold):
```bash
./scripts/run-tlc.sh MCReplicatedRequestConvergence
```

The `Stuck` config is a diagnostic and is EXPECTED to report a violation: it keeps a one-write cap but disables reconnect replay. A lost write or an offline peer therefore reaches the old stuck trace (owner terminal, peer non-terminal, budget exhausted, no enabled fixing action):
```bash
./scripts/run-tlc.sh MCReplicatedRequestConvergenceStuck
```

The `PeerClaim` config is the safety diagnostic and is EXPECTED to report a violation: with the adversarial peer-claim action armed (`AllowPeerClaim = TRUE`), a peer drives itself into `Claimed`, reachably VIOLATING `INVARIANT SingleClaimer`. This proves `SingleClaimer` clause (1) is falsifiable — its green result in the two configs above is evidence the `agent_did` watcher fence holds, not a type artifact:
```bash
./scripts/run-tlc.sh MCReplicatedRequestConvergencePeerClaim
```

The script runs TLC with parallel workers and writes state-graph artifacts to `states/` (gitignored).

## Bounded parameters

Current parameters in `MCReversePairing.cfg`:

| Parameter | Value | Effect of increasing |
|-----------|-------|---------------------|
| `Node` | `{A, B}` | State space grows as |Node|^|Node|; 3-node run is feasible but much slower |
| `Collection` | `{c1}` | The default full crash-recovery bound is single-collection. Use `MCReversePairingMulti.cfg` for a two-collection sanity run |
| `RPCId` | `{r1, r2, r3, r4, r5, r6}` | More ids give more headroom above StateBound; raising without also raising StateBound has little effect; both together increase exploration depth |
| `MaxCrashes` | `2` | Each additional crash budget step multiplies the reachable crash-sequence count; +1 roughly doubles runtime |
| `NoOf` | `NoOf` (sentinel) | Not a tunable — must remain a value disjoint from RPCId |
| `StateBound` | `Cardinality(rpcIdsUsed) <= 4` | Bounds total RPCs ever issued in any trace; raising risks pool exhaustion before liveness converges (a bounded-model artifact, not a real bug) |

Larger parameters increase state space exponentially. State-space-exhaustion artifacts can mask real bugs; benchmark before raising.

`MCReversePairingMulti.cfg` raises only the collection axis and backs off crash interleavings:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `Node` | `{A, B}` | Same peer-pair topology as the default model |
| `Collection` | `{c1, c2}` | Exercises the collection-generic action and liveness quantification across two collections |
| `RPCId` | `{r1, r2, r3, r4, r5, r6}` | Same bounded id pool/headroom as the default model |
| `MaxCrashes` | `0` | Keeps the multi-collection liveness run tractable; crash recovery remains covered by the default single-collection bound |
| `StateBound` | `Cardinality(rpcIdsUsed) <= 4` | Same total-issued-RPC bound as the default model |

Current parameters in `MCSubagentCompletion.cfg`:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `Deployment` | `{A, B}` | Parent bridge row on A, child terminal authority on B |
| `ParentDeployment` / `ChildDeployment` | `A` / `B` | Avoids hard-coding deployment symbols inside the model |
| `Child` | `{c1, c2}` | Two background children are enough to exercise wake-up coalescing |
| `EventId` | `{e1, e2, e3, e4}` | Bounded document-gossip ids; `StateBound` permits three consumed ids |
| `QueueId` | `{q1, q2}` | One user request plus one reusable automated wake-up row |
| `MaxCrashes` | `1` | A and B can each crash once |
| `MaxDrops` | `1` | One document-gossip observation can be dropped before fair re-emission |
| `StateBound` | `Cardinality(eventIdsUsed) <= 3 /\ Cardinality(queueIdsUsed) <= 2` | Leaves enough room for two child terminals plus one dropped observation while cutting off arbitrary duplicate-event churn |

Current parameters in `MCSubagentCancelPropagation.cfg`:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `Deployment` | `{A, B}` | Parent bridge/cancel intent on A, child request owner on B |
| `ParentDeployment` / `ChildDeployment` | `A` / `B` | Avoids hard-coding deployment symbols inside the model |
| `Child` | `{c1}` | One live child edge is enough for the default cancel-delivery liveness run |
| `RPCId` | `{r1, r2, r3, r4, r5, r6}` | Bounded cancel and ack attempt ids |
| `MaxCrashes` | `1` | A and B can each crash once |
| `MaxDrops` | `1` | One cancel or ack RPC can be dropped before fair retry/delivery |
| `StateBound` | `\A child : ~cancelHandledB[child] => FreshIds(1)` | Excludes finite-id-pool exhaustion before B can allocate the handling ack; real RPC ids are unbounded |

Current parameters in `MCReplicatedRequestConvergence.cfg` / `MCReplicatedRequestConvergenceStuck.cfg`:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `Owner` | `o` | The owning node — the request's `agent_did` holder and the only node that drives the lifecycle |
| `ReplicaHolder` | `{p1}` | The one requesting coordinator authorized to hold the host-owned child request; unrelated fleet peers are excluded |
| `DeltaId` | `{d1, d2, d3, d4, d5, d6}` | Bounded id pool for terminal re-emissions; `StateBound` caps consumption below the pool size |
| `MaxDrops` | `1` | One terminal delta may drop before fair re-emission converges (green) / strands a peer (stuck) |
| `MaxCrashes` | `1` | One total node crash (owner restart or a peer losing volatile inbound) |
| `TerminalKind` | `{Completed, Failed}` | The peer must converge to the *specific* terminal the owner reached, not merely some terminal |
| `Cap` | `3` (green) / `1` (stuck) | Persisted per-request budget = shipping `TERMINAL_REDRIVE_CAP`; each emission is one owner write fanned out only to online request-party replicators. The green config also enables reconnect replay, so a party peer offline beyond all three writes still converges. |
| `ReplayOnRecovery` | `TRUE` (green/peer-claim) / `FALSE` (stuck) | Production recovery reinstalls a configured subagent replicator once per reconnect, causing a bounded full replay. The stuck diagnostic removes that action and exposes the exhausted-cap liveness gap. |
| `StateBound` | `Cardinality(deltaIdsUsed) <= Cap` | `emitCount` is persisted per request and caps same-value owner writes at `Cap`, independent of replica count. |

Current parameters in `MCP2PBackpressureGreen.cfg` / diagnostic configs:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `Peer` | `{slow, good1, good2}` | One nonresponsive replicator plus two healthy peers is the smallest hub fan-out that can show slot starvation. |
| `ResponsivePeer` | `{good1, good2}` | Healthy peers must eventually receive the current push wave. `slow` can only time out/fail. |
| `InboundPeer` | `{good1, good2}` | Two inbound senders are enough to fill a one-entry pending-DAG map and exercise the overflow behavior. |
| `PushWorkers` | `1` | Smallest semaphore bound that makes timeout-slot release load-bearing. Larger bounds mask the diagnostic until enough slow peers fill them. |
| `MaxPending` | `1` | Smallest pending-DAG capacity that can show a success-ack at capacity discarding tracking state. |
| `TimeoutReleasesSlot` | `TRUE` green / `FALSE` timeout diagnostic | Models whether a timed-out outbound PushLog releases its semaphore slot. |
| `AckWithoutPendingAllowed` | `FALSE` green / `TRUE` bad-ack diagnostic | Arms the forbidden success-ack path so `SuccessAckBacked` is falsifiable. |

## What TLC checks

Active lines from `MCReversePairing.cfg`:

- **`INVARIANT TypeOK`** — all seven state variables stay within their declared types throughout every reachable state.
- **`INVARIANT RPCIdsTracked`** — every RPC in `messages`, `inFlight`, or `pendingInbound` has its id recorded in `rpcIdsUsed`; prevents id reuse.
- **`INVARIANT RPCWellFormed`** — every Install/Teardown has `src # tgt` and `of = NoOf`; every Ack has `of \in rpcIdsUsed`; kind is always in the declared set.
- **`PROPERTY Convergence`** — every desired/replicator disagreement either converges (replicator catches up) or the operator retracts the desired entry; see Convergence form below.
- **`CONSTRAINT StateBound`** — not a checked property; truncates state-space exploration at `Cardinality(rpcIdsUsed) <= 4` to keep the bounded RPCId pool from exhausting before convergence completes.

Note: `InFlightJustified` is defined in `ReversePairing.tla` as a documented model property but is not enforced by TLC due to a bounded-model artifact (pool-exhausted states cannot be excluded without parameter changes that explode the state space). See its block comment in the .tla file.

Active lines from `MCSubagentCompletion.cfg`:

- **`INVARIANT TypeOK`** — all state variables stay within their declared bounded domains.
- **`INVARIANT DurableChildTerminalOK`** — any durable child terminal on B has a durable final response.
- **`INVARIANT EventIdsTracked`**, **`ObservationBackedByBDurable`**, **`ADurableObservationBackedByB`** — document-gossip observations and A's durable observations are backed by B's durable terminal/final-response state.
- **`INVARIANT BridgeTerminalUnique`** — every parent bridge row terminalizes at most once.
- **`INVARIANT ProjectionRequiresBDurableTerminal`**, **`ProjectionRequiresADurableObservation`**, **`ProjectionMatchesLeanBridgeMapping`** — A projects only from durable observations and maps child `Completed` to bridge `Completed`, all child non-completed terminals to bridge `Failed`.
- **`INVARIANT CancelledOnlyByParentCancel`**, **`ParentCancelAbsorbsLateTerminal`**, **`CancelRequestedCausal`** — bridge `Cancelled` is parent-cancel only, and late child terminals cannot resurrect a cancelled bridge.
- **`INVARIANT NotificationCausal`** — transcript notifications exist only after child projection.
- **`INVARIANT QueueIdsTracked`**, **`WakeupCoalesced`**, **`CompletionWakeupUnique`**, **`WakeupCausal`**, **`UserPendingPreserved`** — automated wake-ups are coalesced, causal, and cancellation drain cannot terminalize user-originated pending work.
- **`PROPERTY CompletionProgress`** — durable child terminals eventually project or settle to parent cancellation; projected terminals eventually notify; notifications eventually have a pending coalesced wake-up representation.
- **`CONSTRAINT StateBound`** — bounds finite event/queue id consumption to keep TLC from exploring duplicate-id churn that is not meaningful in the real unbounded-id system.

Active lines from `MCSubagentCancelPropagation.cfg`:

- **`INVARIANT TypeOK`** - all state variables stay within their declared bounded domains.
- **`INVARIANT RPCIdsTracked`**, **`RPCWellFormed`** - every cancel/ack RPC is tracked and has the expected A-to-B or B-to-A shape.
- **`INVARIANT CancelIntentCausal`** - attempts, handled cancels, acks, and in-system RPCs are justified by durable A-side cancel intent.
- **`INVARIANT AckRequiresHandled`** - every ack that exists is backed by durable B-side cancel handling.
- **`INVARIANT CancelHandledIdempotent`** - B records at most one durable cancel-handling effect per child.
- **`INVARIANT CascadeInterruptsOnlyRunning`**, **`InterruptedOnlyByCascade`** - `Interrupted` is produced only by cascade cancel handling.
- **`INVARIANT InterruptExactlyOnce`**, **`NaturalTerminalStableAfterCancel`**, **`HandledCancelStable`** - repeated deliveries do not double-write; natural terminals remain stable when late cancel handling arrives.
- **`PROPERTY CancelPropagationProgress`** - durable A-side cancel intent eventually reaches durable B-side handling, and a live child eventually becomes interrupted or naturally terminal.
- **`CONSTRAINT StateBound`** - cuts off the bounded RPCId-pool artifact where no fresh id remains for the first B-side handling ack.

Note: `CancelAckProgress` is defined in `SubagentCancelPropagation.tla` but is not enforced by the default TLC config. It can fail only in bounded-pool-exhausted traces after B has already durably handled the cancel and A has lost the matching in-flight attempt through crash or timeout. The #188 safety boundary is `cancelHandledB`; ack progress is an observability/retry-retirement requirement.

Active lines from `MCPairingTransportDialable.cfg` (`Dialable = TRUE`, `ReplicatorInstallable = TRUE`, all hold):

- **`INVARIANT TypeOK`** — `connState`, `subscribed`, `replicatorInstalled`, `docsReplicated` stay in their declared domains.
- **`INVARIANT PartialApplyHasProgress`** — whenever the link is `Connected` and the control-plane collection is subscribed but the replicator is not yet installed, `InstallReplicator` is still ENABLED. On the healthy path this holds; it is the guard that the MODE B/C diagnostic deliberately violates (see below), catching a partial-apply that has become a silent dead end.
- **`INVARIANT ReplicationImpliesReplicator`** — `docsReplicated` is reachable only through an installed replicator.
- **`PROPERTY ReplicatorLiveness`** (`<>replicatorInstalled`) — under a dialable ticket, an installable replicator, and strong fairness on `DialSucceed`, the replicator eventually installs despite arbitrary intervening dial timeouts.
- **`PROPERTY EndToEndLiveness`** (`<>docsReplicated`) — a document eventually flows initiator→target.

The two diagnostic configs each reproduce one real failure mode (the production grounding for each is in "PairingTransport derived requirements"):

- **`MCPairingTransportUndialable.cfg`** (`Dialable = FALSE`) — **MODE A, the literal #511 hang.** The invariants still hold (the failure is purely liveness; `PartialApplyHasProgress` is *vacuous* here — the `Connected ∧ subscribed` state is never reached). `PROPERTY ReplicatorLiveness` is violated; the counterexample is `Disconnected → Connecting → …` with the connection NEVER reaching `Connected`, so nothing subscribes — faithful to the real connect-FIRST hard gate, where an undialable ticket aborts the whole tick before any op and writes no applied row at all.
- **`MCPairingTransportReplicatorStuck.cfg`** (`Dialable = TRUE`, `ReplicatorInstallable = FALSE`) — **MODE B/C, the durable partial row.** The connection establishes and the collection subscribes, but the replicator install can never succeed (its separate transport dial keeps timing out, or a pre-dial cid/filter check fails). TLC reaches `Connected ∧ subscribed ∧ ¬replicatorInstalled` and reports it as an `INVARIANT PartialApplyHasProgress` violation — that state IS the live "subscribed collections, `replicator_addresses: null`" row, now a *modeled* state rather than only prose. `ReplicatorLiveness` and `EndToEndLiveness` are also violated; `TypeOK` and `ReplicationImpliesReplicator` still hold.

The key structural result: `DialSucceed` requires `Dialable` and `InstallReplicator` requires `ReplicatorInstallable`, and no fairness annotation can enable a disabled action. The reconciler's retries (SF on `Dial`/`Redial`, WF on the install) cannot make an un-dialable ticket dialable or make a non-installable replicator install — both are *transport/materialization preconditions the layer above must supply* (dial the shareable public address, not a listen-form address; resolve every replicated collection's schema so `add_replicator`'s cid lookup and filter validation pass). This is the TLA+ counterpart of the Lean `convergence_requires_successful_install` obligation: a connect/install *failure* step leaves the disagreement count unchanged and `> 0`, so convergence requires a *successful* install, which requires both a live connection and an installable replicator.

Active lines from `MCP2PBackpressureGreen.cfg` (`TimeoutReleasesSlot = TRUE`, `AckWithoutPendingAllowed = FALSE`) — **modeled properties; TLC not yet run** (no recorded-runs row):

- **`INVARIANT TypeOK`** — outbound states and inbound admission sets stay in their declared bounded domains.
- **`INVARIANT PushSlotsBounded`** — in-flight outbound PushLog sends never exceed `PushWorkers`.
- **`INVARIANT PendingBounded`** — registered inbound pending DAGs never exceed `MaxPending`.
- **`INVARIANT SuccessAckBacked`** — every inbound success ack is backed by either a merged block or a pending-DAG registration; overflow must nack.
- **`INVARIANT FailedOnlyUnresponsive`** — healthy peers cannot be marked failed in the current push wave.
- **`PROPERTY HealthyPeersDeliver`** — every responsive peer eventually reaches `Delivered`, even if the nonresponsive peer gets the first worker slot.
- **`PROPERTY InboundSettles`** — every inbound PushLog eventually receives either success or nack.

The diagnostics are **designed** to reproduce one load-bearing failure each (claims, not TLC-verified results):

- **`MCP2PBackpressureTimeoutStall.cfg`** (`TimeoutReleasesSlot = FALSE`) — intended counterexample: start `slow` first, fill the only worker slot, leave healthy peers queued forever so `HealthyPeersDeliver` fails. This encodes **necessity** of slot release in a one-worker toy: if timeout does not free the semaphore, healthy delivery can fail even under fairness. It is **not** a sufficiency claim that Amy hub saturation is solved when the green BOOLEAN holds (production can still fail via multi-slow-peer fill, re-queue storms, Bitswap stalls, or gossip-loop death after the permit is released).
- **`MCP2PBackpressureBadAck.cfg`** (`AckWithoutPendingAllowed = TRUE`) — intended counterexample: fill the pending-DAG map and success-ack a second inbound PushLog without merging or registering it so `SuccessAckBacked` fails. Formal counterpart of the production invariant (defradb.rs #1089 / pinned hub admission) that pending-DAG capacity overflow must return the backpressure nack, not success.

Active lines from `MCReplicatedRequestConvergence.cfg` (`Cap = 3`, reconnect replay enabled):

- **`INVARIANT TypeOK`** — `reqState`, the gossip `messages`/`pendingInbound` sets, `deltaIdsUsed`, and the drop/crash counters stay in their declared domains.
- **`INVARIANT SingleClaimer`** — every non-owner peer only ever holds `Pending` or the owner's terminal (delivered by an owner delta); it never sits in `Claimed`/`Processing`. A peer claiming/processing of its own volition would violate this — it is the model fence for the `agent_did` watcher filter.
- **`INVARIANT DeltaBackedByOwnerTerminal`** — every in-flight terminal delta carries the owner's (absorbing) terminal value; no peer can converge to a value the owner never reached.
- **`INVARIANT DeltaIdsTracked`** — every in-flight delta's id is recorded in `deltaIdsUsed`; no id reuse.
- **`INVARIANT ReplayBound`** — a peer recovery schedules at most one full replay in the modeled reconnect cycle.
- **`PROPERTY TerminalConverges`** (`owner-terminal ~> every request-party peer reflects it`) — online party peers converge through bounded owner writes under bounded loss; a party peer offline beyond the entire cap converges through fair `RecoverPeer` and `ReplayTerminalSnapshot`. Replay consumes no delta id or request-write budget. Unrelated fleet peers are outside `ReplicaHolder` by the #683 scope contract.
- **`CONSTRAINT StateBound`** — bounds total terminal re-emissions to keep TLC from exploring re-drive churn that is not meaningful with unbounded real delta ids.

Two diagnostic configs reproduce the pre-fix convergence gap (liveness) and prove the safety property is falsifiable (safety):

- **`MCReplicatedRequestConvergenceStuck.cfg`** (`Cap = 1`, `ReplayOnRecovery = FALSE`) — the invariants still hold while `PROPERTY TerminalConverges` is intentionally VIOLATED. One owner write can be lost or emitted while a peer is offline; `emitCount = Cap` then disables further writes and replay is unavailable, leaving owner-terminal/peer-nonterminal forever. This proves reconnect replay, rather than an unbounded rewrite loop, is the load-bearing repair for partitions beyond the cap.
- **`MCReplicatedRequestConvergencePeerClaim.cfg`** (`AllowPeerClaim = TRUE`) — arms the `PeerClaimsForeign` action, which drives a peer from `Pending` into `Claimed` of its own volition (the exact thing the `agent_did` watcher filter forbids, and what a peer-side write to a foreign replica would do). `INVARIANT SingleClaimer` is intentionally VIOLATED. Without this config, `SingleClaimer` clause (1) (`reqState[n] ∉ {Claimed, Processing}`) would be vacuously true — no other action can put a peer in those states — so its green result would prove nothing. With a config in which the property actually breaks, the green runs above are real evidence the fence holds. Mirrors the `Cap` red/green that makes `TerminalConverges` load-bearing.

## Fairness annotations

The spec uses two fairness flavors:
- **Weak fairness (WF)** on `Deliver`, `Process`, `ReceiveAck`, `Timeout`: action eventually fires if continuously enabled.
- **Strong fairness (SF)** per-node on `Reconcile`: action eventually fires if enabled infinitely often.

Notes:
- WF on `Timeout` is essential — without it, dropped RPCs strand `inFlight` permanently and Reconcile cannot re-emit (its `~PendingInstallFor` precondition stays false). The plan originally said "no fairness on Timeout" — that guidance was wrong; this README is the correct reference.
- SF (not WF) on `Reconcile` is required because `OperatorWrite` can transiently disable individual `(p, c)` reconcile preconditions, defeating WF's "continuously enabled" requirement. SF's "infinitely often enabled" matches a real periodic reconcile loop.
- No fairness on `Drop`, `Crash`, or `OperatorWrite` — those are voluntary actions that the model can skip.

`SubagentCompletion` uses weak fairness on the recovery/projection workers:

- per-child `EmitTerminalObservation`
- document delivery and A-side durable observation persistence
- per-child bridge projection, notification append, and wake-up enqueue

It deliberately has no fairness on child terminal writes, document drops, crashes, parent cancellation, cancellation drain, or user request enqueue.

`SubagentCancelPropagation` uses weak fairness on:

- per-child `EmitCancel`
- cancel/ack `Deliver`
- B-side `ProcessCancel`
- A-side `ReceiveAck`
- A-side `Timeout`

It deliberately has no fairness on `InvokeBridgeCancelCascade`, `NaturalTerminalize`, `Drop`, or `Crash`.

`ReplicatedRequestConvergence` uses weak fairness on the bounded repair workers:

- `EmitTerminalDelta` (one owner write fanned out to online replicators)
- `DeliverDelta` and `PersistDeltaOnPeer`
- per-peer `RecoverPeer` and `ReplayTerminalSnapshot`

It deliberately has no fairness on the owner lifecycle (`Claim`/`Process`/`Terminalize`), `DropDelta`, `CrashPeer`, or `CrashOwner` — those are voluntary. `EmitTerminalDelta` is gated by persisted `emitCount < Cap`, never by observing peer state. Fair reconnect and full replay are therefore the unbounded-partition liveness assumption. The `Stuck` config removes replay while retaining the bounded write budget and reaches the exhausted-cap disagreement.

## Convergence form

`InstallConverges` and `TeardownConverges` use a disjunctive leads-to form:

```
(c \in desired[n][p] /\ n # p) ~> (c \in replicator[p][n] \/ c \notin desired[n][p])
```

This says: every disagreement either converges OR the operator retracts. The strict form `P ~> Q` is provably violated by traces where the operator changes `desired` before `Reconcile` completes — which is allowed at any time in this model (`OperatorWrite` has no fairness). The disjunctive form is the standard "progress at every observed disagreement" pattern for systems with unconstrained operator input. The vacuity probe (Task 10 review) confirms it is non-trivial: TLC explores stable-`desired` traces and requires `Q` along them.

## Expected output

### Safety only (drop the PROPERTY line, keep INVARIANT lines)

```
Model checking completed. No error has been found.
... N states generated, M distinct states found, 0 states left on queue.
The depth of the complete state graph search is K.
```

For default parameters: ~M in the high hundreds of thousands, runtime under 60 seconds.

### Full run with liveness (default)

Same final line, plus a "Checking temporal properties" phase. Total runtime: completes in under 5 minutes on a 2024 laptop at default parameters.

A failure looks like:

```
Error: Temporal property Convergence was violated.
The behavior up to this violation:
  State 1 ... State 2 ... ...
```

Read the trace from top to bottom; identify the action between each state pair. Look for the divergence point — the state where progress stalled.

## Recorded runs

Reference environment for the following runs: macOS arm64, OpenJDK 17.0.19, TLC v1.8.0, `-workers auto` using 18 workers.

| Config | Bound | Result | State space | Depth | Runtime |
|--------|-------|--------|-------------|-------|---------|
| `MCReversePairing.cfg` | `Collection = {c1}`, `MaxCrashes = 2` | Passes `TypeOK`, `RPCIdsTracked`, `RPCWellFormed`, `Convergence` | 322,560 distinct states | 19 | 3min 21s |
| `MCReversePairingMulti.cfg` | `Collection = {c1, c2}`, `MaxCrashes = 0` | Passes `TypeOK`, `RPCIdsTracked`, `RPCWellFormed`, `Convergence` | 2,164,720 distinct states; 28,085,121 generated | 18 | 36min 38s |
| `MCSubagentCompletion.cfg` | `Child = {c1, c2}`, `MaxCrashes = 1`, `MaxDrops = 1`, `StateBound = eventIdsUsed <= 3 /\ queueIdsUsed <= 2` | Passes all listed SubagentCompletion invariants and `CompletionProgress` | 787,112 distinct states; 5,752,621 generated | 20 | 2min 01s |
| `MCSubagentCancelPropagation.cfg` | `Child = {c1}`, `MaxCrashes = 1`, `MaxDrops = 1`, `StateBound = unhandled child retains one fresh RPC id` | Passes all listed SubagentCancelPropagation invariants and `CancelPropagationProgress` | 416,230 distinct states; 1,651,727 generated | 21 | 11s |
| `MCPairingTransportDialable.cfg` | `Dialable = TRUE`, `ReplicatorInstallable = TRUE` | Passes `TypeOK`, `PartialApplyHasProgress`, `ReplicationImpliesReplicator`, `ReplicatorLiveness`, `EndToEndLiveness` | 7 distinct states; 8 generated | — | <1s (OpenJDK 25) |
| `MCPairingTransportUndialable.cfg` | `Dialable = FALSE`, `ReplicatorInstallable = TRUE` | **MODE A diagnostic:** invariants hold (`PartialApplyHasProgress` vacuous); `ReplicatorLiveness` intentionally VIOLATED (never-`Connected` trace = connect-fails-first hang) | 3 distinct states; 4 generated | — | <1s (OpenJDK 25) |
| `MCPairingTransportReplicatorStuck.cfg` | `Dialable = TRUE`, `ReplicatorInstallable = FALSE` | **MODE B/C diagnostic:** `TypeOK`/`ReplicationImpliesReplicator` hold; `PartialApplyHasProgress` intentionally VIOLATED at `Connected ∧ subscribed ∧ ¬installed` (the partial row); `ReplicatorLiveness`/`EndToEndLiveness` also violated | 5 distinct states; 6 generated | — | <1s (OpenJDK 25) |
| `MCReplicatedRequestConvergence.cfg` | `Cap = 3`, `ReplayOnRecovery = TRUE`, one request-party replica | Passes `TypeOK`, `SingleClaimer`, delta/replay invariants, and `TerminalConverges` | 294 distinct states; 728 generated | 13 | <1s |
| `MCReplicatedRequestConvergenceStuck.cfg` | `Cap = 1`, `ReplayOnRecovery = FALSE` | Invariants hold; `TerminalConverges` intentionally VIOLATED with the requester offline and the persisted budget exhausted | 42 distinct states; 78 generated | 8 | <1s |
| `MCReplicatedRequestConvergencePeerClaim.cfg` | `AllowPeerClaim = TRUE` | `SingleClaimer` intentionally VIOLATED by `PeerClaimsForeign` | 47 distinct states before counterexample; 51 generated | 6 | <1s |

The multi-collection run's final temporal-property pass dominated runtime: TLC completed BFS first, then checked 16 temporal branches over 34,635,520 total distinct states in 19min 27s.

The SubagentCompletion run used TLC2 `2026.05.12.170007` (rev `8033878`) with OpenJDK 17.0.19 on macOS arm64, `-workers auto` using 18 workers. TLC checked 8 temporal branches; the final temporal phase took 1min 16s.

The SubagentCancelPropagation run used TLC2 `2026.05.12.170007` (rev `8033878`) with OpenJDK 17.0.19 on macOS arm64, `-workers auto` using 18 workers. TLC checked 2 temporal branches; the final temporal phase took 3s.

The three ReplicatedRequestConvergence runs used TLC2
`2026.07.03.221739` (rev `227f61b`) with Homebrew OpenJDK 26.0.1 on macOS
arm64, `-workers auto` using 18 workers. The one-requester green run checked
one temporal branch; the other two are deliberately failing diagnostics.

Crash-enabled two-collection attempts were stopped for tractability, not property failure:

- `Collection = {c1, c2}`, `MaxCrashes = 1`: stopped after 30min 51s while checking a temporal-property phase over 81,451,952 total distinct states. Last BFS progress before that phase was 4,825,260 distinct states, 38,397,144 generated, 2,273,059 queued, depth 13.
- `Collection = {c1, c2}`, `MaxCrashes = 2`: stopped after 30min 24s while checking a temporal-property phase over 143,512,080 total distinct states. Last BFS progress before that phase was 8,706,840 distinct states, 66,727,226 generated, 4,367,182 queued, depth 14.

## Known limitations and follow-ups

- **ReplicatedRequestConvergence replay abstraction.** The model treats a
  configured reconnect replay as one atomic application of the owner's current
  terminal head. Rust binds this to delete+reinstall of the existing filtered
  replicator; DefraDB transport authorization, filter validity, and eventual
  replay completion remain external assumptions. The connection tracker covers
  local dial, remote-first reconnect, and daemon startup, but permanent
  disconnect or permanent reinstall failure is outside the liveness claim.
- **Sub-sweep connection flaps.** `peerOnline` does not model a peer that drops
  and reconnects entirely between two reconciler samples. Production likewise
  detects inactive-to-active edges only at sweeps (although document-update
  events normally trigger extra sweeps during redrive). A partition that lasts
  beyond the request-write cap but disappears wholly between samples can evade
  reconnect replay and leave a stale terminal replica. Closing that residual
  requires a transport reconnect event or independent anti-entropy signal, not
  a stronger claim about the current polling tracker.
- **Crash-enabled multi-collection scope.** `MCReversePairingMulti.cfg` verifies `Collection = {c1, c2}` with `MaxCrashes = 0`. Attempts with one or two crashes did not fail, but crossed the reference runtime cutoff during liveness checking. The leads-to property is parametric in `(n, p, c)`, so the multi-collection no-crash run is a sanity check for collection-generic structure rather than a replacement for the default crash-recovery bound.
- **Per-action SF on Reconcile.** Current `\A n \in Node : SF_vars(Reconcile(n))` enforces fairness on each node's reconcile loop but treats the disjunction over `(p, c)` as one action. Multi-collection liveness may need per-(p, c) fairness: `\A n, p \in Node, c \in Collection : SF_vars(ReconcileInstall(n, p, c) \/ ReconcileTeardown(n, p, c))`. **Follow-up:** add when multi-collection runs are needed.
- **StateBound constraint.** `Cardinality(rpcIdsUsed) <= 4` bounds total RPCs ever issued in any trace. This avoids the bounded-pool artifact but limits exploration depth in long-running traces. **Follow-up:** lift the bound or replace with a different bound (e.g., per-cycle limits) once the model is stable.
- **InFlightJustified not TLC-checked.** The supporting invariant is defined as a documented model property but commented out in the .cfg because it fails in pool-exhausted states (a bounded-model artifact, not a real bug). **Follow-up:** TLAPS proof, or a parameter regime that avoids the artifact.
- **Provenance.** Not modeled here. The structural-safety invariants check actions are well-formed, but a full provenance proof — every replicator entry traces back to a prior `desired`-then-`Process` chain — is a future TLAPS effort.
- **Set semantics for `messages`.** Assumes RPC ids are unique (which the model enforces). Real network duplicates can be modeled via `Send` re-emitting under different ids.
- **N > 2 nodes, data-plane convergence, authorization correctness.** Explicit non-goals per the spec.
- **SubagentCompletion foreground mode.** The model covers background completion projection only. Foreground cross-deployment blocking is a separate liveness surface.
- **SubagentCompletion cancel propagation.** `SubagentCompletion` still records only `cancelRequested[child]`; delivery of that cascade interrupt is modeled in the sibling `SubagentCancelPropagation` artifact.
- **SubagentCompletion duplicate-event bound.** The default config permits two child terminal observations plus one dropped/re-emitted observation. Arbitrary duplicate document churn is cut off by `StateBound`; the real system relies on unbounded event ids and idempotent projection.
- **SubagentCompletion queue abstraction.** A drained automated wake-up row is reactivated for later completion instead of accumulating historical drained rows. The checked properties are pending-row uniqueness, wake-up causality, and user-row preservation, not queue audit history.
- **Unsafe early projection counterexample.** Not committed as a separate failing config. The counterexample is direct: if A projects from `pendingInboundA` before `PersistObservationOnA`, then `ProjectionRequiresADurableObservation` fails immediately, and an A crash would erase the only local evidence for the bridge terminal.
- **SubagentCancelPropagation default fanout.** The default liveness config models one child. The action and properties are child-parametric, but a two-child sanity bound is left for future work if R5 needs explicit fanout coverage.
- **SubagentCancelPropagation ack progress.** `CancelAckProgress` is defined but excluded from the default config because finite RPCId exhaustion can strand ack retirement after B durable handling has already satisfied the #188 delivery boundary.
- **SubagentCancelPropagation foreground mode and detach policy.** Parent foreground progress and detach semantics remain separate follow-ups.

## SubagentCompletion derived requirements

The SubagentCompletion model surfaces these implementation obligations:

1. A's projection worker must consume durable local child terminal/final-response rows, not volatile subscription callbacks.
2. B must persist the child final response before or atomically with the child terminal, and terminal observability must wait until both documents are durable.
3. Completion wake-up enqueue needs atomic coalescing for `(session_id, queue_key, pending)`.
4. Projection, notification append, and wake-up enqueue must be idempotent under duplicate observations and retries.
5. Transcript notification must be durable before the wake-up request is represented.
6. Cancellation drain must filter automated `subagent_completion` rows and preserve user-originated pending work.
7. Late child terminal after parent cancellation is a no-op for the parent bridge.

## SubagentCancelPropagation derived requirements

The SubagentCancelPropagation model surfaces these implementation obligations:

1. A must persist cascade intent before relying on remote cancel delivery.
2. A recovery/retry worker must re-emit cancel RPCs from durable intent after timeout, drop, or crash.
3. B must durably handle the cancel before emitting an ack.
4. B-side cancel handling must be idempotent under duplicate RPCs.
5. A live child interrupted by cascade reaches `Interrupted` exactly once.
6. A child that naturally terminalizes before cancel delivery keeps that natural terminal; late cancel handling is absorbed.
7. Timeout is liveness-only and must not infer or mutate child terminal state.
8. A ack receipt is useful observability/retry retirement, but B durable handling is the safety boundary.

## PairingTransport derived requirements

The model distinguishes three production failure modes, grounded in `reconcile_peer_tick` (engine.rs) + `add_replicator` (defradb.rs iroh) + `parse_public_peer_addr`:

- **MODE A — connect-fails-first** (`Dialable = FALSE`). `admin.connect(addresses)` is a connect-FIRST hard gate (`?` before the diff). An undialable listen-form ticket (parse yields no direct addrs under no-relay/no-discovery) fails connect, the whole tick aborts, and NO `PeerPairingApplied` row is written — the sweep just retries every 30s. This is the literal #511 fleet hang.
- **MODE B — connect OK, replicator dial fails** (`ReplicatorInstallable = FALSE`). connect succeeds (the ticket is dialable), the InstallCollection ops persist per-op, then `add_replicator`'s *own, separate* transport dial times out. Same ticket as connect, so the shareable-address fix covers it.
- **MODE C — connect OK, replicator pre-dial check fails** (`ReplicatorInstallable = FALSE`, same modeled state as B). `add_replicator` resolves every replicated collection's cid and validates filters BEFORE its dial; a missing collection schema (`not_found`) or filter-validation failure produces the same partial row, and the shareable-address fix does **NOT** cover it.

Implementation obligations:

1. The address a peer dials must be the **shareable public address** (a reachable direct addr), not a listen-form / under-specified address. Under no-relay + no-discovery there is no fallback resolution path, so an un-dialable address is a permanent liveness failure (MODE A), not a slow one. Fix site: invite-token construction must use the shareable form on *every* path (both the live-daemon and persisted-token branches), and the endpoint heartbeat must publish the shareable form.
2. The reconciler must keep retrying (the model's SF on `Dial`/`Redial`, WF on the install); a single connect/install failure must not be terminal. Aborting the current tick on connect failure and retrying next tick is acceptable — it matches the Lean `dialFailed` fixpoint — *provided* the precondition eventually holds.
3. Dialability and replicator-installability are preconditions the layer above the reconciler must guarantee; the reconcile loop cannot manufacture either. A reconciler stuck at subscribed-but-no-replicator violates `PartialApplyHasProgress` — partial apply must remain visibly incomplete and retryable, never silently "done".
4. End-to-end replication is reachable only through an installed replicator (`ReplicationImpliesReplicator`); a green control-plane subscription is not evidence that data will flow.
5. **For the #511 cut-6 e2e: the "subscribed collections, null replicator" partial row is AMBIGUOUS between MODE B (fixed by the shareable address) and MODE C (a schema/filter materialization bug, NOT fixed). Diagnose by log signature — `"Address Lookup failed"` / dial timeout = A/B; `collection not found` / filter-validation error = C — before concluding the address fix was insufficient.**

## Refining the model

If TLC finds a real bug:

1. Inspect the counterexample trace — which action transitions led to violation?
2. Decide: is the model wrong (over-permissive transition relation) or is the property over-stated?
3. Fix the model and re-run; never silently weaken the property to make a violation go away.
4. Document the diagnosis in the commit message.
