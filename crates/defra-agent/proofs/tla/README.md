# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `ReversePairing` — control-plane convergence of subscription/replicator reverse-pairing between two peers. Spec design: `docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md` (removed from the tree; see git history). Implementation plan: `docs/superpowers/plans/2026-05-08-reverse-pairing-tla-spec.md` (removed from the tree; see git history).
- `SubagentCompletion` — background subagent terminal projection where the parent bridge row lives on deployment A and the child terminalizes on deployment B. Spec design: `docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md` (removed from the tree; see git history). Implementation plan: `docs/superpowers/plans/2026-05-12-subagent-completion-cross-deployment-tla-spec.md` (removed from the tree; see git history).
- `SubagentCancelPropagation` - cascade-cancel delivery from a parent bridge row on deployment A to the child request owner on deployment B. Spec design: `docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-tla-design.md` (removed from the tree; see git history). Implementation plan: `docs/superpowers/plans/2026-05-13-subagent-cancel-propagation-tla.md` (removed from the tree; see git history).
- `PairingTransport` — connection establishment + replication liveness for one directed pairing edge, the transport layer *below* `ReversePairing`. `ReversePairing` and the Lean `PairingReconcile` model both assume the transport carries the install RPC / that `connect` succeeds; neither models *establishing* the link, so the live #511 fleet hang was outside the modeled world. This spec closes that gap. It distinguishes the **three real failure modes** (grounded in the production code) with two independent BOOLEAN constants — `Dialable` (the connect-gate ticket form) and `ReplicatorInstallable` (whether `add_replicator` can succeed once connected) — exercised by a **three-config diagnostic**: `MCPairingTransportDialable` (both true: the shareable-address fix — all properties hold); `MCPairingTransportUndialable` (MODE A — `Dialable = FALSE`: connect-fails-first, the *literal* #511 hang, never reaches `Connected` so nothing subscribes and no applied row is written); `MCPairingTransportReplicatorStuck` (MODE B/C — `ReplicatorInstallable = FALSE`: connect OK and collections subscribe but the replicator install never succeeds — the durable "subscribed collections, `replicator_addresses` null" partial row). Companion to the Lean `PairingReconcile` `dialFailed` (MODE A) and `reconcileInstallReplicatorFailed` (MODE B/C) transitions and the `convergence_requires_successful_install` obligation.
- `ReplicatedRequestConvergence` — terminal-state convergence of a replicated `AgentRequest` document across an owning node and its non-owning peer replicas (issue #664, incident #661). SAFETY (`SingleClaimer`) is the existing `agent_did` watcher filter (`watcher/query.rs`): peers are strictly passive — a peer's only lifecycle action is applying a delivered owner delta, so it never claims/processes a foreign replica of its own volition. LIVENESS (`TerminalConverges`) is the fix: because DefraDB has no per-doc anti-entropy re-drive on a running peer (defradb.rs#1074) and recovery is owner-scoped + startup-only, a one-shot terminal PushLog that drops leaves peers permanently stuck non-terminal. The model abstracts the design's **owner re-drive** (the Phase-0 Rust binding — idempotent same-value terminal re-assert) as an `EmitTerminalDelta` action bounded by a per-peer budget `Cap` — **faithful to the shipping `TERMINAL_REDRIVE_CAP`**: the owner re-asserts each peer at most `Cap` times *without observing whether it converged* (there is no back-channel), then stops. `TerminalConverges` is therefore a **conditional theorem** — it holds iff the budget `Cap` exceeds the delivery loss a peer suffers (`MaxDrops + MaxCrashes`), which is exactly the shipping behavior: within the cap window bounded loss is tolerated, and beyond it the code falls back to the next organic write (out of model scope). Two documented modeling assumptions: (i) at most one re-assert per peer is outstanding at a time, reflecting that the 5s reconcile ticks are spaced so each PushLog resolves before the next; (ii) a peer replica is modeled as receiving only *terminal* deltas — in production it also carries the owner's replicated *intermediate* states (`Claimed`/`Processing`, the literal #661 shape), which are benign (owner-originated, never self-claimed) and orthogonal to terminal convergence, so they are not modeled. Exercised by a **three-config diagnostic** driven by `Cap` (liveness) and `AllowPeerClaim` (safety falsifiability): `MCReplicatedRequestConvergence` (`Cap = 3` = the shipping cap, `MaxDrops = 1`, `MaxCrashes = 1`, `AllowPeerClaim = FALSE` — budget outlasts the loss: `SingleClaimer` holds AND `TerminalConverges` holds); `MCReplicatedRequestConvergenceStuck` (`Cap = 1` — budget too small for the loss: a single dropped emission strands a peer, `TerminalConverges` reachably VIOLATED — the shipping cap's failure mode when a peer's losses exceed its budget); `MCReplicatedRequestConvergencePeerClaim` (`AllowPeerClaim = TRUE` — arms the adversarial `PeerClaimsForeign` action: a peer drives itself into `Claimed`, reachably VIOLATING `SingleClaimer` — this proves the safety property's clause (1) is not vacuous, so its green result elsewhere is real evidence the `agent_did` fence holds). Single-node conformance fences the owner re-drive binding; multi-node e2e (`tests/e2e_lifecycle/replicated_request_convergence_p2p_e2e.rs`) exercises a real second node applying intermediate + terminal owner deltas over P2P and the re-drive re-push after a late peer join. Spec design: `docs/superpowers/specs/2026-07-08-replicated-request-convergence-664-design.md`.
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

For ReplicatedRequestConvergence — the green model checks clean (both properties hold):
```bash
./scripts/run-tlc.sh MCReplicatedRequestConvergence
```

The `Stuck` config is a diagnostic and is EXPECTED to report a violation: with the budget too small for the loss (`Cap = 1`), a peer's single re-assert is dropped and its budget is spent, stranding it — violating `TerminalConverges` with the reachable stuck trace (owner terminal, peer non-terminal, budget exhausted, no enabled fixing action). This is the shipping cap's failure mode when a peer's losses exceed its re-emit budget:
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
| `ReplicaHolder` | `{p1, p2}` | Two non-owning peer replicas — the minimum to make convergence non-trivial across more than one holder |
| `DeltaId` | `{d1, d2, d3, d4, d5, d6}` | Bounded id pool for terminal re-emissions; `StateBound` caps consumption below the pool size |
| `MaxDrops` | `1` | One terminal delta may drop before fair re-emission converges (green) / strands a peer (stuck) |
| `MaxCrashes` | `1` | One total node crash (owner restart or a peer losing volatile inbound) |
| `TerminalKind` | `{Completed, Failed}` | The peer must converge to the *specific* terminal the owner reached, not merely some terminal |
| `Cap` | `3` (green) / `1` (stuck) | Per-peer re-emit budget = the shipping `TERMINAL_REDRIVE_CAP`. `3` outlasts `MaxDrops + MaxCrashes = 2` losses → converges; `1` is smaller than the loss → strands a peer. The theorem is conditional on `Cap > loss`. |
| `StateBound` | `Cardinality(deltaIdsUsed) <= Cap * Cardinality(ReplicaHolder)` | Redundant safety net: `emitCount` already caps each peer at `Cap`, so total re-emissions never exceed `Cap * |ReplicaHolder|` |

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

Active lines from `MCReplicatedRequestConvergence.cfg` (`Cap = 3`, both properties hold):

- **`INVARIANT TypeOK`** — `reqState`, the gossip `messages`/`pendingInbound` sets, `deltaIdsUsed`, and the drop/crash counters stay in their declared domains.
- **`INVARIANT SingleClaimer`** — every non-owner peer only ever holds `Pending` or the owner's terminal (delivered by an owner delta); it never sits in `Claimed`/`Processing`. A peer claiming/processing of its own volition would violate this — it is the model fence for the `agent_did` watcher filter.
- **`INVARIANT DeltaBackedByOwnerTerminal`** — every in-flight terminal delta carries the owner's (absorbing) terminal value; no peer can converge to a value the owner never reached.
- **`INVARIANT DeltaIdsTracked`** — every in-flight delta's id is recorded in `deltaIdsUsed`; no id reuse.
- **`PROPERTY TerminalConverges`** (`owner-terminal ~> every peer reflects it`) — CONDITIONAL on the budget: with `Cap = 3` outlasting the `MaxDrops + MaxCrashes = 2` loss, and `WF` fairness on `EmitTerminalDelta`/`DeliverDelta`/`PersistDeltaOnPeer`, every replica holder eventually converges to the owner's terminal. The budget, not fairness alone, is what closes the gap — see the `Stuck` config where `Cap = 1 < loss` and convergence fails.
- **`CONSTRAINT StateBound`** — bounds total terminal re-emissions to keep TLC from exploring re-drive churn that is not meaningful with unbounded real delta ids.

Two diagnostic configs reproduce the pre-fix convergence gap (liveness) and prove the safety property is falsifiable (safety):

- **`MCReplicatedRequestConvergenceStuck.cfg`** (`Cap = 1`) — the invariants still hold (the failure is purely liveness). The budget is too small for the loss: each peer gets one re-assert, and `PROPERTY TerminalConverges` is intentionally VIOLATED when that single emission is dropped (`dropCount = 1`) — `emitCount[peer] = 1 = Cap` disables further re-emit, so with no in-flight delta and no budget left that peer stutters forever at `Pending` while the owner is terminal. This is the model-level "owner=terminal, replica=non-terminal, budget exhausted, no enabled fixing action" stuck state — the shipping cap's real failure mode when a peer's losses exceed its budget. It is NOT converged by fairness (the fixing action is *disabled*, not merely unfair); only a budget larger than the loss (green, `Cap = 3`) converges. This is the faithful counterpart of the code's `TERMINAL_REDRIVE_CAP`: beyond the budget, convergence relies on the next organic write, which is out of model scope.
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

`ReplicatedRequestConvergence` uses weak fairness on the owner re-drive workers only:

- per-peer `EmitTerminalDelta` (the re-emittable owner terminal re-drive)
- `DeliverDelta` and `PersistDeltaOnPeer`

It deliberately has no fairness on the owner lifecycle (`Claim`/`Process`/`Terminalize`), `DropDelta`, `CrashPeer`, or `CrashOwner` — those are voluntary. This isolation is the load-bearing result: `TerminalConverges` holds only because `EmitTerminalDelta` is weakly fair *and* its per-peer budget `Cap` exceeds the loss; the `Stuck` config keeps the identical fairness but sets `Cap = 1 < loss`, and convergence fails — proving the re-emit *budget*, not the fairness alone, is what closes the gap. Because `EmitTerminalDelta` is gated by `emitCount[peer] < Cap` (never by whether the peer converged), the model's fairness cannot manufacture convergence the shipping cap would not deliver.

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

The multi-collection run's final temporal-property pass dominated runtime: TLC completed BFS first, then checked 16 temporal branches over 34,635,520 total distinct states in 19min 27s.

The SubagentCompletion run used TLC2 `2026.05.12.170007` (rev `8033878`) with OpenJDK 17.0.19 on macOS arm64, `-workers auto` using 18 workers. TLC checked 8 temporal branches; the final temporal phase took 1min 16s.

The SubagentCancelPropagation run used TLC2 `2026.05.12.170007` (rev `8033878`) with OpenJDK 17.0.19 on macOS arm64, `-workers auto` using 18 workers. TLC checked 2 temporal branches; the final temporal phase took 3s.

Crash-enabled two-collection attempts were stopped for tractability, not property failure:

- `Collection = {c1, c2}`, `MaxCrashes = 1`: stopped after 30min 51s while checking a temporal-property phase over 81,451,952 total distinct states. Last BFS progress before that phase was 4,825,260 distinct states, 38,397,144 generated, 2,273,059 queued, depth 13.
- `Collection = {c1, c2}`, `MaxCrashes = 2`: stopped after 30min 24s while checking a temporal-property phase over 143,512,080 total distinct states. Last BFS progress before that phase was 8,706,840 distinct states, 66,727,226 generated, 4,367,182 queued, depth 14.

## Known limitations and follow-ups

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
