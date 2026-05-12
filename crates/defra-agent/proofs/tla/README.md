# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `ReversePairing` — control-plane convergence of subscription/replicator reverse-pairing between two peers. Spec design: `../../../../docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md`. Implementation plan: `../../../../docs/superpowers/plans/2026-05-08-reverse-pairing-tla-spec.md`.
- `SubagentCompletion` — background subagent terminal projection where the parent bridge row lives on deployment A and the child terminalizes on deployment B. Spec design: `../../../../docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md`. Implementation plan: `../../../../docs/superpowers/plans/2026-05-12-subagent-completion-cross-deployment-tla-spec.md`.
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

The multi-collection run's final temporal-property pass dominated runtime: TLC completed BFS first, then checked 16 temporal branches over 34,635,520 total distinct states in 19min 27s.

The SubagentCompletion run used TLC2 `2026.05.12.170007` (rev `8033878`) with OpenJDK 17.0.19 on macOS arm64, `-workers auto` using 18 workers. TLC checked 8 temporal branches; the final temporal phase took 1min 16s.

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
- **SubagentCompletion cancel propagation.** `cancelRequested[child]` is written and checked for causality, but delivery of the cascade interrupt to B is deferred.
- **SubagentCompletion duplicate-event bound.** The default config permits two child terminal observations plus one dropped/re-emitted observation. Arbitrary duplicate document churn is cut off by `StateBound`; the real system relies on unbounded event ids and idempotent projection.
- **SubagentCompletion queue abstraction.** A drained automated wake-up row is reactivated for later completion instead of accumulating historical drained rows. The checked properties are pending-row uniqueness, wake-up causality, and user-row preservation, not queue audit history.
- **Unsafe early projection counterexample.** Not committed as a separate failing config. The counterexample is direct: if A projects from `pendingInboundA` before `PersistObservationOnA`, then `ProjectionRequiresADurableObservation` fails immediately, and an A crash would erase the only local evidence for the bridge terminal.

## SubagentCompletion derived requirements

The SubagentCompletion model surfaces these implementation obligations:

1. A's projection worker must consume durable local child terminal/final-response rows, not volatile subscription callbacks.
2. B must persist the child final response before or atomically with the child terminal, and terminal observability must wait until both documents are durable.
3. Completion wake-up enqueue needs atomic coalescing for `(session_id, queue_key, pending)`.
4. Projection, notification append, and wake-up enqueue must be idempotent under duplicate observations and retries.
5. Transcript notification must be durable before the wake-up request is represented.
6. Cancellation drain must filter automated `subagent_completion` rows and preserve user-originated pending work.
7. Late child terminal after parent cancellation is a no-op for the parent bridge.

## Refining the model

If TLC finds a real bug:

1. Inspect the counterexample trace — which action transitions led to violation?
2. Decide: is the model wrong (over-permissive transition relation) or is the property over-stated?
3. Fix the model and re-run; never silently weaken the property to make a violation go away.
4. Document the diagnosis in the commit message.
