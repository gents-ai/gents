# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `ReversePairing` — control-plane convergence of subscription/replicator reverse-pairing between two peers.
- `PairingTransport` — connection establishment + replication liveness for one directed pairing edge, the transport layer *below* `ReversePairing`. `ReversePairing` and the Lean `PairingReconcile` model both assume the transport carries the install RPC / that `connect` succeeds; neither models *establishing* the link, so the live #511 fleet hang was outside the modeled world. This spec closes that gap. It distinguishes the **three real failure modes** (grounded in the production code) with two independent BOOLEAN constants — `Dialable` (the connect-gate ticket form) and `ReplicatorInstallable` (whether `add_replicator` can succeed once connected) — exercised by a **three-config diagnostic**: `MCPairingTransportDialable` (both true: the shareable-address fix — all properties hold); `MCPairingTransportUndialable` (MODE A — `Dialable = FALSE`: connect-fails-first, the *literal* #511 hang, never reaches `Connected` so nothing subscribes and no applied row is written); `MCPairingTransportReplicatorStuck` (MODE B/C — `ReplicatorInstallable = FALSE`: connect OK and collections subscribe but the replicator install never succeeds — the durable "subscribed collections, `replicator_addresses` null" partial row). Companion to the Lean `PairingReconcile` `dialFailed` (MODE A) and `reconcileInstallReplicatorFailed` (MODE B/C) transitions and the `convergence_requires_successful_install` obligation.
- `P2PBackpressure` — hub fan-in/fan-out admission **obligation** model for #630, not a multi-wave flood-safety proof. `PairingTransport` proves a single edge can connect/install; this model starts after that. Its one-wave necessity obligations say outbound timeouts release push-worker slots and inbound success acks are backed by modeled pending registration or merge. The pinned DefraDB implementation has since added bounded queue admission before worker execution, per-peer scheduling/coalescing, persisted retry handoff on overflow, and durable push-originated pending-DAG recovery. Those multi-wave and restart properties, plus Bitswap stalls, rate limits, gossip-loop health, multi-slow-peer fill, and peer-vs-CID pending, remain outside this model. Receiver-side pending-DAG slot starvation and acknowledged-DAG TTL loss belong to the defradb.rs receiver dispatch model (`proofs/tla/SyncOwnership.tla`, `MC_SyncOwnership_Dispatch` configs), not here. See `boundary.p2p-backpressure.obligation-model`. Green/red configs (`MCP2PBackpressureGreen` / `TimeoutStall` / `BadAck`) are **modeled, not yet TLC-checked**.
- `BackgroundAdmissionContention` — concurrent writers of one request's
  `AgentRequest` document and session documents during a turn (#3592). Each
  accepted `spawn_process` call runs a dispatch start (reads the request to
  fence on its lease, moves its own row to running), an admission
  transaction (reads the request's `AgentToolCall` rows and fence, bumps
  `AgentRequest.background_admission_sequence`, creates the child) and a
  receipt transaction (rereads the tool rows and request, takes the next
  `AgentMessage` sequence, refreshes the `AgentSession` observation); running
  children write those session-scoped documents when they finish, and the
  execution lease renewal task CASes the request's deadline at each due
  poll beside the turn. Commits follow DefraDB's optimistic rule: a commit
  aborts when a document it read or wrote changed after its snapshot (writers
  of different fields of one request document conflict, as the
  `request_row_writers` tests show), and a transaction whose bounded attempts
  all abort is lost. `BudgetBound`, `NoConflictLoss`, `ReceiptOnce`,
  `LeaseLive` and `EveryCallSettles` hold in the owned loop's order, where
  `run_batch` runs a turn's calls one at a time, under the renewal cadence
  premise (Lean `consecutive_renewals_obey_cadence`: at most one renewal lands
  in one call's attempt series), and with the renewal owner re-driving a poll
  whose attempts all abort. The renewal guarantee is the weaker one: a poll
  can be exhausted by admission bumps (`RenewalWitness`), and only the
  re-drive keeps the lease live. `LeaseLapse` is the found race: waiting for
  the next poll, as the code did, leaves a lease period no due poll with
  margin, and sequential admissions lapse the lease while the turn runs.
  `Concurrent` admits every sibling at once and loses a call to conflicts
  (the #3592 regression schedule); `Unfenced` drops the sequence bump and
  overfills the budget; `ShortBudget` gives `Max` attempts and loses a call to
  its running children; `RenewalBudget` gives `Max + 1` and loses an
  admission to the children plus one renewal, so the second extra attempt of
  the Lean `receiptContentionAttempts` bound is the renewal's; `Unspaced`
  drops the cadence premise and loses an admission to back-to-back renewals;
  `StartWitness` reproduces the traced renewal/dispatch-start conflict that
  green absorbs.
- `ClientSubmissionContention` — a User's signed client submission racing
  writes to the documents it reads. The reservation
  (`request.client.reserve_session`) binds the session and selected Agent the
  User signs; the submission (`request.client.submit`) reads the
  `AgentSession` document and creates the User-signed `AgentRequest`. The
  working Agent refreshes the session observation as it publishes each
  message, so either phase's commit aborts. `NoConflictLoss`, `CreatedOnce`
  and `EverySubmissionSettles` hold when both phases are signed-conflict
  rebuilds (Lean `clientSignedWriteMayRetry`: only a confirmed abort
  re-reads, re-plans and signs a fresh graph; a lost response is reported)
  and fewer refreshes land in one attempt series than it has attempts.
  `SingleAttempt` is the found submission race (the busy-session mailbox
  test); `ReserveSingleAttempt` is the found reservation race ("a partial read
  lost to a newer merge" in the live backgrounding run); `ShortBudget` shows
  the overlap premise is load-bearing.
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

For BackgroundAdmissionContention — the green model (the owned loop's
order, two cadence-spaced lease renewals, re-driven renewal polls) checks
clean, including `EveryCallSettles`:
```bash
./scripts/run-tlc.sh MCBackgroundAdmissionContention
```

Its eight diagnostics are EXPECTED to report violations. `LeaseLapse`
reproduces the renewal race (`LeaseLive`), `Concurrent` reproduces #3592
(`NoConflictLoss`: an admission loses every attempt to sibling commits),
`Unfenced` violates `BudgetBound`, `ShortBudget` violates `NoConflictLoss`
with only running children writing, `RenewalBudget` violates
`NoConflictLoss` with the children and one renewal, `Unspaced` violates
`NoConflictLoss` with back-to-back renewals, `RenewalWitness` violates
`RenewalNeverLost`, and `StartWitness` violates `StartNeverConflicts`:
```bash
./scripts/run-tlc.sh MCBackgroundAdmissionContentionLeaseLapse
./scripts/run-tlc.sh MCBackgroundAdmissionContentionConcurrent
./scripts/run-tlc.sh MCBackgroundAdmissionContentionUnfenced
./scripts/run-tlc.sh MCBackgroundAdmissionContentionShortBudget
./scripts/run-tlc.sh MCBackgroundAdmissionContentionRenewalBudget
./scripts/run-tlc.sh MCBackgroundAdmissionContentionUnspaced
./scripts/run-tlc.sh MCBackgroundAdmissionContentionRenewalWitness
./scripts/run-tlc.sh MCBackgroundAdmissionContentionStartWitness
```

Each run directly, from this directory, is:
```bash
java -cp <tla2tools.jar> tlc2.TLC -workers 2 -metadir <scratch dir> \
  -config MCBackgroundAdmissionContention.cfg MCBackgroundAdmissionContention
```
Delete any `*_TTrace_*` files a failing diagnostic writes here.

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

## Fairness annotations

The spec uses two fairness flavors:
- **Weak fairness (WF)** on `Deliver`, `Process`, `ReceiveAck`, `Timeout`: action eventually fires if continuously enabled.
- **Strong fairness (SF)** per-node on `Reconcile`: action eventually fires if enabled infinitely often.

Notes:
- WF on `Timeout` is essential — without it, dropped RPCs strand `inFlight` permanently and Reconcile cannot re-emit (its `~PendingInstallFor` precondition stays false). The plan originally said "no fairness on Timeout" — that guidance was wrong; this README is the correct reference.
- SF (not WF) on `Reconcile` is required because `OperatorWrite` can transiently disable individual `(p, c)` reconcile preconditions, defeating WF's "continuously enabled" requirement. SF's "infinitely often enabled" matches a real periodic reconcile loop.
- No fairness on `Drop`, `Crash`, or `OperatorWrite` — those are voluntary actions that the model can skip.

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
| `MCPairingTransportDialable.cfg` | `Dialable = TRUE`, `ReplicatorInstallable = TRUE` | Passes `TypeOK`, `PartialApplyHasProgress`, `ReplicationImpliesReplicator`, `ReplicatorLiveness`, `EndToEndLiveness` | 7 distinct states; 8 generated | — | <1s (OpenJDK 25) |
| `MCPairingTransportUndialable.cfg` | `Dialable = FALSE`, `ReplicatorInstallable = TRUE` | **MODE A diagnostic:** invariants hold (`PartialApplyHasProgress` vacuous); `ReplicatorLiveness` intentionally VIOLATED (never-`Connected` trace = connect-fails-first hang) | 3 distinct states; 4 generated | — | <1s (OpenJDK 25) |
| `MCPairingTransportReplicatorStuck.cfg` | `Dialable = TRUE`, `ReplicatorInstallable = FALSE` | **MODE B/C diagnostic:** `TypeOK`/`ReplicationImpliesReplicator` hold; `PartialApplyHasProgress` intentionally VIOLATED at `Connected ∧ subscribed ∧ ¬installed` (the partial row); `ReplicatorLiveness`/`EndToEndLiveness` also violated | 5 distinct states; 6 generated | — | <1s (OpenJDK 25) |
| `MCBackgroundAdmissionContention.cfg` | `N = 4`, `Max = 2`, `Attempts = 4`, `StartAttempts = RenewAttempts = 4`, `Renewals = 2`, `RenewTicks = 1`, sequential, sequence bump, re-driven polls, cadence-spaced renewals | Passes `TypeOK`, `BudgetBound`, `NoConflictLoss`, `ReceiptOnce`, `LeaseLive`, `EveryCallSettles` | 417,891 distinct states; 918,452 generated | 48 | 13s |
| `MCBackgroundAdmissionContentionLeaseLapse.cfg` | as green, `Renewals = 1`, `RedriveLostPoll = FALSE` | `LeaseLive` intentionally VIOLATED: the admission bumps of calls 1 to 4 abort all 4 attempts of the one due poll while call 4's receipt is pending | 76,748 distinct states before counterexample; 154,916 generated | 33 | 1s |
| `MCBackgroundAdmissionContentionConcurrent.cfg` | as green, concurrent admission, `Renewals = 0` | `NoConflictLoss` intentionally VIOLATED: call 1's admission aborts on all 4 attempts while siblings and their children commit | 6,698,408 distinct states before counterexample; 19,430,220 generated | 21 | 38s |
| `MCBackgroundAdmissionContentionUnfenced.cfg` | concurrent, no sequence bump, `Attempts = 100`, `Renewals = 0` | `BudgetBound` intentionally VIOLATED: concurrent admissions all count one live row and commit | 26,480 distinct states before counterexample; 55,615 generated | 13 | <1s |
| `MCBackgroundAdmissionContentionShortBudget.cfg` | sequential, `Attempts = 2`, `Renewals = 0` | `NoConflictLoss` intentionally VIOLATED: two running children finish during call 2's receipt | 230 distinct states before counterexample; 302 generated | 21 | <1s |
| `MCBackgroundAdmissionContentionRenewalBudget.cfg` | as green, `Attempts = 3` (`Max + 1`), `Renewals = 1` | `NoConflictLoss` intentionally VIOLATED: two children finish and one renewal commits during call 2's receipt; with `Renewals = 0` the same bound passes | 4,382 distinct states before counterexample; 8,084 generated | 21 | <1s |
| `MCBackgroundAdmissionContentionUnspaced.cfg` | as green, `RenewalSpaced = FALSE` | `NoConflictLoss` intentionally VIOLATED: two back-to-back renewals and two children exhaust call 2's 4 receipt attempts | 33,653 distinct states before counterexample; 68,174 generated | 25 | <1s |
| `MCBackgroundAdmissionContentionRenewalWitness.cfg` | green constants | `RenewalNeverLost` intentionally VIOLATED: admission bumps exhaust a renewal poll, which green's re-drive recovers | 286,417 distinct states before counterexample; 580,008 generated | 33 | 1s |
| `MCBackgroundAdmissionContentionStartWitness.cfg` | green constants | `StartNeverConflicts` intentionally VIOLATED: a renewal commits while call 1's dispatch start is open, the traced conflict | 46 distinct states before counterexample; 94 generated | 7 | <1s |

The nine BackgroundAdmissionContention rows used TLC2 `2026.11.06.222357`
(rev `d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and
`-workers 2`. The green run checked its temporal property over the complete
state space. The counts before each counterexample vary with the worker
count.

The multi-collection run's final temporal-property pass dominated runtime: TLC completed BFS first, then checked 16 temporal branches over 34,635,520 total distinct states in 19min 27s.

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
