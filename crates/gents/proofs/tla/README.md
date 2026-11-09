# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `PairingTransport` — connection establishment + replication liveness for one directed pairing edge, the transport layer below the reconciler. The Lean `PairingReconcile` model assumes the transport carries the install RPC / that `connect` succeeds; it does not model *establishing* the link, so the live #511 fleet hang was outside the modeled world. This spec closes that gap. It distinguishes the **three real failure modes** (grounded in the production code) with two independent BOOLEAN constants — `Dialable` (the connect-gate ticket form) and `ReplicatorInstallable` (whether `add_replicator` can succeed once connected) — exercised by a **three-config diagnostic**: `MCPairingTransportDialable` (both true: the shareable-address fix — all properties hold); `MCPairingTransportUndialable` (MODE A — `Dialable = FALSE`: connect-fails-first, the *literal* #511 hang, never reaches `Connected` so nothing subscribes and no applied row is written); `MCPairingTransportReplicatorStuck` (MODE B/C — `ReplicatorInstallable = FALSE`: connect OK and collections subscribe but the replicator install never succeeds — the durable "subscribed collections, `replicator_addresses` null" partial row). Companion to the Lean `PairingReconcile` `dialFailed` (MODE A) and `reconcileInstallReplicatorFailed` (MODE B/C) transitions and the `convergence_requires_successful_install` obligation.
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
- `PackUxWatch` — a pack page's live data watch (`desktop_ux_data_watch`)
  racing runtime restarts, window node selection, unwatch, window
  destruction and four runtime hazards (stopped for good, credentials
  refused, a watcher that fails after every connect, malformed events). The
  task re-acquires the endpoint at every connect, backs off on transient
  failures, keeps its failure count across accepted connections, ends the
  watch after `RetryBudget` consecutive failures, notifies the page of every
  failed attempt and every end, and sends under the registry lock only while
  its watch is registered; the selection change cancels before it writes the
  selection. `NoCrossNodeDelivery`, `RegistryReleased`, `NotSilent`,
  `TerminalNamed`, `EventualResume`, `NoLeakedWatch` and `EntryReleased` hold
  with every guard. `Transient` is the observed defect (one read timeout ends
  the watch); `StaleEndpoint` reuses the endpoint captured at start;
  `NodeSwitch` drops the selection cancel; `SelectBeforeCancel` writes the
  selection before cancelling; `UnlockedDeliver` checks the token and then
  sends outside the lock; `NoDestroyCancel` keeps a destroyed window's
  registry entry; `SilentBackoff` sends only snapshots and terminal errors;
  `ResetOnAccept` resets the count on every accepted connection; `Unbounded`
  retries forever. Lean `ClientShell.PackWatch` owns the per-task phases.
- `SessionHeadObservation` — a session's observed request head and the
  client that projects it on reopen (#3604). The runtime claims a session's
  requests one at a time; each claim or admission refusal names the incoming
  request as `AgentSession.observation.latest_request` only when it is the
  canonical latest (Lean `AgentSession.advance`), and a completion refreshes
  the head only when it already names it. Writers commit in a serial order
  (DefraDB's optimistic validation re-reads aborted transactions). The
  reopened client reads the runtime as its User in two snapshots: nonterminal
  requests with the observed head, then the head row by document ID; a load
  overlapping a change event is discarded and re-run. `created_at` is
  whole-second and `request_id` random, so a successor admitted in the second
  its predecessor finished has no order of its own. `HeadFollowsBoundary` and
  `ReplyObserved` hold under the Lean `AgentSession.newer` order (live over
  terminal, then later terminalization, then `request_id`).
  `RequestIdTie`/`RequestIdTieLiveness` are the found race: the claim loses
  the random tie, the head keeps naming the finished predecessor, and the
  reopened client never shows the successor's reply. `NoLivePreference`,
  `NoTerminalizationOrder` (a same-second refusal) and `NoHeadSeed` (a reader
  without the head read) break each remaining guard. A refusal admitted while
  another turn is live deliberately leaves the live turn as the head, so
  neither property claims it.
- `RuntimeConnectionTransport` — the desktop observer's authenticated SSE
  connection to one runtime it reads as the User (`client/observe`). The
  source task arms `K` subscriptions one at a time and then sends a ready
  notice; every renewal period (weakly fair) it arms a replacement set while
  the current set keeps streaming, swaps them and sends `renewed`
  (make-before-break); a stream ends by a transport failure or a denial. The
  observer applies only notices of the current source generation, runs one
  snapshot at a time, and re-probes a denied source after the Lean
  `reprobeDelayMs`; reconcile replaces or drops the source when the peer's
  endpoint changes or the peer is removed, and an aborted task may still
  enqueue one notice. `Revocation` is how the runtime answers a revoked User:
  `"served"` is the pinned served runtime (handshake accepted, streams
  silent, reads omit the selected roots; observe test
  `revoked_user_is_answered_with_withheld_roots`), which only the renewal
  resnapshot reveals; `"handshake"` and `"envelope"` are the access owner's
  other classified surfaces. `ConnectedAfterArmedSnapshot`,
  `StaleNoticeInert`, `RemovalClears`, `NoEndpointNamed`,
  `RenewalNeverDisconnects`, `ReprobeBounded`, `EventuallyConnected`,
  `DenialObserved` and `NoEndpointEventuallyNamed` hold for all three. The
  diagnostics are the found defects and broken guards: `WithheldRoot` and
  `WithheldRootLiveness` (a withheld root read as a transport failure shows
  a revoked User an outage forever), `BreakBeforeMake` (a renewal that drops
  the armed set shows Connected with nothing armed), `RenewalFlap` (a
  renewal reported as a transport failure), `NoEndpoint`,
  `StaleGeneration` and `EarlyReady`. Durations are not modeled: Lean
  `renewalStartMs` spaces renewals a full period after each completed arm.
- `ObserverResync` — the desktop observer's delivery and resync loop for
  runtime documents and the direct refresh beside it. A runtime write emits a
  subscription event only after the stream is armed; the observer queues the
  node, starts one load that captures the store fence, reads and publishes
  through the fence; a direct refresh does the same once per user mutation.
  `NoLostWrite`, `NoStaleCommit`, `NoReadWithoutReason`,
  `EventualPublication` and `Convergence` hold when request events stay off
  the reload fence, are scoped to their source node, a rejected load requeues
  without back-off and a rejected refresh hands its node to the observer (Lean
  `ClientDataPublication.acceptsReloadFor`, `ObserverResync`).
  `NoReadWithoutReason` bounds hosted reads by batches of reasons to read, not
  below them: with a lease renewal during every load the node is read back to
  back. `Widen` adds loads whose first publication overwrites an unowned row
  and moves every node's fence (Lean `reloadFootprint`,
  `reload_converts_unowned_rows`); a widened load checks only its own node's
  fence (`reloadCheck`). `PersistentWiden` is the footprint computed from the
  raw snapshot: one node's steady widened publications reject every load of
  another, with or without the 250 ms wait. `WidenedCheckAll` makes a widened
  load check every scope, so another node's steady publications reject it
  forever. `RejectDelay` restores the wait in the widened model and still
  passes: the delay is a latency choice, not a guard.
  `SustainedEvents` is the found livelock: one lease renewal during every
  load, with `observe_request_event(None)` moving the fence every reload
  checks, rejects every load forever and the node stays connecting with stale
  rows. `CrossNode` is the same with another node's events. `ReconnectGap`
  keeps the code before the fix without renewals as a regression guard for
  arm-then-snapshot. `NodeScopeOnly`, `NoPublicationFence`, `NoEventQueue`,
  `SnapshotBeforeArm` and `RefreshFallback` each break one remaining guard.
- `DesktopUpdateListener` — the last leg of the desktop's event path: the
  bridge emitter (`spawn_client_update_task`: coalesced watch wakes, one
  `desktop://client-updated` event each), Tauri delivery (an event with no
  registered handler is dropped, never replayed), the frontend's asynchronous
  listener registration, the projection drain loop and the selected session's
  load phases. A read sees the versions committed before it starts. A
  selected request that has not reached its owner answers `pending`, and the
  runtime may catch up with no store notice at all, since the desktop holds no
  replica of a remote runtime's session. A waiting phase is `pending` or one
  of the three delivery phases. `ViewConverges` and `NoStuckPending` hold
  with five guards: a read without the listener once a registration rejects
  or a deadline passes, a full read once the listener resolves, re-listening after a rejected registration,
  bounded re-reads of a waiting phase followed by the retry action (Lean
  `ClientSessionLoad.phase_has_exit`), and a visible failure for a reply for
  another session. `MovedReadQueued` checks that a read whose selection moved
  needs no guard of its own. `NoRegistration` drops registration fairness and
  still settles the selection; `NoInitialRead` shows what the startup read
  prevents there. `RegistrationGap` is the observed defect (a commit between
  the read without a listener and registration is dropped and the view stays a version
  behind); `ListenFailure`, `PendingNoEvent` and `SupersededMismatch` each
  drop one remaining guard.
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

For SessionHeadObservation — the green model (Lean order, head-seeded
reader, claim and refusal paths, both random id orders) checks clean,
including `ReplyObserved`:
```bash
./scripts/run-tlc.sh MCSessionHeadObservation
```

Its five diagnostics are EXPECTED to report violations. `RequestIdTie`
violates `HeadFollowsBoundary` and `RequestIdTieLiveness` violates
`ReplyObserved` with the #3604 schedule (A claimed and completed, B claimed
in the same second, the client reopens, B completes and never appears);
`NoLivePreference`, `NoTerminalizationOrder` and `NoHeadSeed` violate
`ReplyObserved`:
```bash
./scripts/run-tlc.sh MCSessionHeadObservationRequestIdTie
./scripts/run-tlc.sh MCSessionHeadObservationRequestIdTieLiveness
./scripts/run-tlc.sh MCSessionHeadObservationNoLivePreference
./scripts/run-tlc.sh MCSessionHeadObservationNoTerminalizationOrder
./scripts/run-tlc.sh MCSessionHeadObservationNoHeadSeed
```

For RuntimeConnectionTransport — the three green models check clean,
including the three liveness properties:
```bash
./scripts/run-tlc.sh MCRuntimeConnectionTransport
./scripts/run-tlc.sh MCRuntimeConnectionTransportEnvelope
./scripts/run-tlc.sh MCRuntimeConnectionTransportHandshake
```

Its seven diagnostics are EXPECTED to report violations. `WithheldRoot` and
`RenewalFlap` violate `RenewalNeverDisconnects`, `WithheldRootLiveness`
violates `DenialObserved`, `BreakBeforeMake` and `EarlyReady` violate
`ConnectedAfterArmedSnapshot`, `NoEndpoint` violates `NoEndpointNamed` and
`StaleGeneration` violates `StaleNoticeInert`:
```bash
./scripts/run-tlc.sh MCRuntimeConnectionWithheldRoot
./scripts/run-tlc.sh MCRuntimeConnectionWithheldRootLiveness
./scripts/run-tlc.sh MCRuntimeConnectionBreakBeforeMake
./scripts/run-tlc.sh MCRuntimeConnectionRenewalFlap
./scripts/run-tlc.sh MCRuntimeConnectionNoEndpoint
./scripts/run-tlc.sh MCRuntimeConnectionStaleGeneration
./scripts/run-tlc.sh MCRuntimeConnectionEarlyReady
```

For ObserverResync — the green model (two nodes, a renewal during every
load on both, a write, a drop and a direct refresh per node) and the widened
green model check clean; `ReconnectGap` and `RejectDelay` are passing
diagnostics:
```bash
./scripts/run-tlc.sh MCObserverResync
./scripts/run-tlc.sh MCObserverResyncWiden
./scripts/run-tlc.sh MCObserverResyncReconnectGap
./scripts/run-tlc.sh MCObserverResyncRejectDelay
```

Its ten diagnostics are EXPECTED to report violations: `SustainedEvents`,
`CrossNode`, `NodeScopeOnly`, `PersistentWiden` and `WidenedCheckAll`
violate `EventualPublication` and `Convergence`; `NoPublicationFence`,
`NoEventQueue` and `SnapshotBeforeArm` violate `NoLostWrite`;
`StaleCommit` violates `NoStaleCommit`; `RefreshFallback` violates
`NoReadWithoutReason`:
```bash
./scripts/run-tlc.sh MCObserverResyncSustainedEvents
./scripts/run-tlc.sh MCObserverResyncCrossNode
./scripts/run-tlc.sh MCObserverResyncNodeScopeOnly
./scripts/run-tlc.sh MCObserverResyncPersistentWiden
./scripts/run-tlc.sh MCObserverResyncWidenedCheckAll
./scripts/run-tlc.sh MCObserverResyncNoPublicationFence
./scripts/run-tlc.sh MCObserverResyncNoEventQueue
./scripts/run-tlc.sh MCObserverResyncSnapshotBeforeArm
./scripts/run-tlc.sh MCObserverResyncStaleCommit
./scripts/run-tlc.sh MCObserverResyncRefreshFallback
```

For DesktopUpdateListener — the green model (every guard on, one rejected
registration, one bridge mismatch, one selection change, silent and noticed
arrival) checks clean, including both liveness properties, and so does
`NoRegistration`, where registration may never resolve:
```bash
./scripts/run-tlc.sh MCDesktopUpdateListener
./scripts/run-tlc.sh MCDesktopUpdateListenerNoRegistration
```

Its five diagnostics are EXPECTED to report violations. `RegistrationGap`
and `ListenFailure` violate `ViewConverges`; `NoInitialRead`,
`PendingNoEvent` and `SupersededMismatch` violate `NoStuckPending`:
```bash
./scripts/run-tlc.sh MCDesktopUpdateListenerNoInitialRead
./scripts/run-tlc.sh MCDesktopUpdateListenerRegistrationGap
./scripts/run-tlc.sh MCDesktopUpdateListenerListenFailure
./scripts/run-tlc.sh MCDesktopUpdateListenerPendingNoEvent
./scripts/run-tlc.sh MCDesktopUpdateListenerSupersededMismatch
```

For PackUxWatch — the green model checks clean; the nine diagnostics are
EXPECTED to report the violation named in their config:
```bash
./scripts/run-tlc.sh MCPackUxWatch
./scripts/run-tlc.sh MCPackUxWatchTransient
./scripts/run-tlc.sh MCPackUxWatchStaleEndpoint
./scripts/run-tlc.sh MCPackUxWatchNodeSwitch
./scripts/run-tlc.sh MCPackUxWatchSelectBeforeCancel
./scripts/run-tlc.sh MCPackUxWatchUnlockedDeliver
./scripts/run-tlc.sh MCPackUxWatchNoDestroyCancel
./scripts/run-tlc.sh MCPackUxWatchSilentBackoff
./scripts/run-tlc.sh MCPackUxWatchResetOnAccept
./scripts/run-tlc.sh MCPackUxWatchUnbounded
```

Each run directly, from this directory, is:
```bash
java -cp <tla2tools.jar> tlc2.TLC -workers 2 -metadir <scratch dir> \
  -config MCBackgroundAdmissionContention.cfg MCBackgroundAdmissionContention
```
Delete any `*_TTrace_*` files a failing diagnostic writes here.

The script runs TLC with parallel workers and writes state-graph artifacts to `states/` (gitignored).

## Bounded parameters


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


## Recorded runs

Reference environment for the following runs: macOS arm64, OpenJDK 17.0.19, TLC v1.8.0, `-workers auto` using 18 workers.

| Config | Bound | Result | State space | Depth | Runtime |
|--------|-------|--------|-------------|-------|---------|
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
| `MCSessionHeadObservation.cfg` | `Order = "lean"`, `HeadSeed = TRUE`, `Refusals = TRUE` | Passes `TypeOK`, `HeadFollowsBoundary`, `ReplyObserved` | 838 distinct states; 1,406 generated | 11 | <1s |
| `MCSessionHeadObservationRequestIdTie.cfg` | `Order = "requestId"`, `Refusals = FALSE` | `HeadFollowsBoundary` intentionally VIOLATED: ClaimA, CompleteA, AdmitB, ClaimB leaves the head on A | 77 distinct states before counterexample; 99 generated | 6 | <1s |
| `MCSessionHeadObservationRequestIdTieLiveness.cfg` | `Order = "requestId"`, `Refusals = FALSE` | `ReplyObserved` intentionally VIOLATED: the #3604 schedule; after reopen B completes and every load omits it | 620 distinct states; 1,056 generated | 11 | <1s |
| `MCSessionHeadObservationNoLivePreference.cfg` | `Order = "noLive"`, `Refusals = FALSE` | `ReplyObserved` intentionally VIOLATED: B's claim ranks below A's same-second terminalization | 620 distinct states; 1,056 generated | 11 | <1s |
| `MCSessionHeadObservationNoTerminalizationOrder.cfg` | `Order = "noTerminal"`, `Refusals = TRUE` | `ReplyObserved` intentionally VIOLATED: B refused in A's second loses the random tie | 838 distinct states; 1,406 generated | 11 | <1s |
| `MCSessionHeadObservationNoHeadSeed.cfg` | `Order = "lean"`, `HeadSeed = FALSE` | `ReplyObserved` intentionally VIOLATED: the head is B but the reader reads only nonterminal rows | 572 distinct states; 984 generated | 11 | <1s |
| `MCRuntimeConnectionTransport.cfg` | `K = 2`, `MaxGen = 4`, `Cap = 2`, `MaxEnv = 3`, `MaxChan = 2`, `Revocation = "served"`, all guards | Passes `TypeOK`, `ConnectedAfterArmedSnapshot`, `RemovalClears`, `NoEndpointNamed`, `RenewalNeverDisconnects`, `ReprobeBounded`, `StaleNoticeInert`, `EventuallyConnected`, `DenialObserved`, `NoEndpointEventuallyNamed` | 65,879 distinct states; 432,424 generated | 54 | 5s |
| `MCRuntimeConnectionTransportEnvelope.cfg` | as green, `Revocation = "envelope"` | Passes every invariant and property of the green row | 73,847 distinct states; 482,536 generated | 54 | 5s |
| `MCRuntimeConnectionTransportHandshake.cfg` | as green, `Revocation = "handshake"` | Passes every invariant and property of the green row | 48,429 distinct states; 320,510 generated | 40 | 4s |
| `MCRuntimeConnectionWithheldRoot.cfg` | as green, `WithheldClassified = FALSE` | `RenewalNeverDisconnects` intentionally VIOLATED: revoked grant, renewal resnapshot, withheld root read as a transport failure, Disconnected at a reachable runtime | 3,801 distinct states before counterexample; 15,907 generated | 9 | <1s |
| `MCRuntimeConnectionWithheldRootLiveness.cfg` | as `WithheldRoot`, liveness only | `DenialObserved` intentionally VIOLATED: the revoked User loops through disconnected resnapshots and never reaches denied | 39,775 distinct states; 288,098 generated | 32 | 3s |
| `MCRuntimeConnectionBreakBeforeMake.cfg` | as green, `MakeBeforeBreak = FALSE` | `ConnectedAfterArmedSnapshot` intentionally VIOLATED: Connected while the renewal re-arm has no subscription armed | 3,564 distinct states before counterexample; 14,923 generated | 9 | <1s |
| `MCRuntimeConnectionRenewalFlap.cfg` | as green, `RenewalAsFailure = TRUE` | `RenewalNeverDisconnects` intentionally VIOLATED: arm, ready, renewal, failure notice, Disconnected with the runtime reachable | 2,228 distinct states before counterexample; 8,158 generated | 8 | <1s |
| `MCRuntimeConnectionNoEndpoint.cfg` | as green, `NamedNoEndpoint = FALSE` | `NoEndpointNamed` intentionally VIOLATED: the peer loses its endpoint and reconcile leaves no status | 50 distinct states before counterexample; 111 generated | 4 | <1s |
| `MCRuntimeConnectionStaleGeneration.cfg` | as green, `GenerationCheck = FALSE` | `StaleNoticeInert` intentionally VIOLATED: the replaced source's queued notice moves the new source's status | 220 distinct states before counterexample; 596 generated | 5 | <1s |
| `MCRuntimeConnectionEarlyReady.cfg` | as green, `ArmBeforeReady = FALSE` | `ConnectedAfterArmedSnapshot` intentionally VIOLATED: ready before any subscription is armed, snapshot, Connected | 909 distinct states before counterexample; 2,713 generated | 6 | <1s |
| `MCObserverResync.cfg` | 2 nodes, `MaxWrites = 1`, `K = 1`, `MaxDrops = 1`, `MaxRefreshes = 1`, node-scoped events off the reload fence, immediate requeue, no refresh fallback, no widening | Passes `TypeOK`, `NoLostWrite`, `NoReadWithoutReason`, `NoStaleCommit`, `EventualPublication`, `Convergence` | 2,835,856 distinct states; 16,250,601 generated | 33 | 7min 5s |
| `MCObserverResyncWiden.cfg` | 2 nodes, renewals during every load on both, each node's first publication widens (`MaxWidens = 1`), widened loads check their own node | Passes every invariant and property | 23,689 distinct states; 100,043 generated | 34 | 2s |
| `MCObserverResyncRejectDelay.cfg` | `Widen` with the 250 ms wait after a rejected load | Passes every invariant and property: the wait is not a guard | 25,109 distinct states; 105,521 generated | 36 | 2s |
| `MCObserverResyncSustainedEvents.cfg` | 1 node, renewals during every load, events touch every scope on the reload fence, 250 ms retry | `EventualPublication` and `Convergence` intentionally VIOLATED: Arm, Start, Write, Deliver, Read, Publish (rejected), Timer, then Start, Renew, Deliver, Read, Publish (rejected) forever | 118 distinct states; 229 generated | 17 | <1s |
| `MCObserverResyncCrossNode.cfg` | 2 nodes, only `b` renews, events touch every scope | `EventualPublication` and `Convergence` intentionally VIOLATED for `a`: each of `b`'s events rejects `a`'s in-flight load | 10,322 distinct states; 33,989 generated | 38 | 1s |
| `MCObserverResyncPersistentWiden.cfg` | 2 nodes, `a` renews and every publication of `a` widens, 250 ms retry | `EventualPublication` and `Convergence` intentionally VIOLATED for `b`: each of `a`'s widened publications rejects `b`'s in-flight load | 4,615 distinct states; 15,136 generated | 29 | <1s |
| `MCObserverResyncWidenedCheckAll.cfg` | 2 nodes, `b` renews, `a`'s first publication widens, a widened load checks every scope | `EventualPublication` and `Convergence` intentionally VIOLATED for `a`: each of `b`'s publications rejects `a`'s widened load | 5,652 distinct states; 18,225 generated | 25 | <1s |
| `MCObserverResyncReconnectGap.cfg` | 2 nodes, `MaxWrites = 2`, `MaxDrops = 2`, no renewals, the code before the fix | Passes every invariant and property: writes between a drop and its re-arm are read by the post-arm snapshot | 626,865 distinct states; 2,738,115 generated | 33 | 43s |
| `MCObserverResyncNodeScopeOnly.cfg` | 1 node, node-scoped events still on the reload fence, immediate requeue | `EventualPublication` and `Convergence` intentionally VIOLATED: the node's own renewals reject each load | 112 distinct states; 220 generated | 17 | <1s |
| `MCObserverResyncNoPublicationFence.cfg` | 1 node, publications do not fence reloads; `NoStaleCommit` unchecked | `NoLostWrite` intentionally VIOLATED: the observer publishes its newer read, then the older direct refresh publishes over it | 140 distinct states before counterexample; 253 generated | 11 | <1s |
| `MCObserverResyncStaleCommit.cfg` | `NoPublicationFence` checking only `TypeOK` and `NoStaleCommit` | `NoStaleCommit` intentionally VIOLATED: the same older refresh replaces the newer published version | 118 distinct states before counterexample; 207 generated | 9 | <1s |
| `MCObserverResyncNoEventQueue.cfg` | 1 node, a delivered event does not queue its node | `NoLostWrite` intentionally VIOLATED: a write after the snapshot read is delivered and never loaded | 21 distinct states before counterexample; 25 generated | 7 | <1s |
| `MCObserverResyncSnapshotBeforeArm.cfg` | 1 node, the snapshot is queued before the streams arm | `NoLostWrite` intentionally VIOLATED: a write between the read and the arm has no event and no later load | 65 distinct states before counterexample; 102 generated | 7 | <1s |
| `MCObserverResyncRefreshFallback.cfg` | 1 node, a rejected refresh reads again | `NoReadWithoutReason` intentionally VIOLATED: the observer's publication rejects the refresh, which reads its node a second time | 114 distinct states before counterexample; 176 generated | 9 | <1s |
| `MCDesktopUpdateListener.cfg` | `MaxVersion = 2`, `ListenFailures = 1`, `Mismatches = 1`, `Selections = 1`, `RereadBudget = 2`, every guard on | Passes `TypeOK`, `MovedReadQueued`, `ViewConverges`, `NoStuckPending` | 10,819 distinct states; 46,787 generated | 18 | <1s |
| `MCDesktopUpdateListenerNoRegistration.cfg` | as green, `SpecListenerUnfair` (registration may never resolve) | Passes `TypeOK`, `MovedReadQueued`, `NoStuckPending`: the read without a listener settles the selection with no event at all | 10,819 distinct states; 46,787 generated | 18 | <1s |
| `MCDesktopUpdateListenerNoInitialRead.cfg` | as NoRegistration, `InitialRead = FALSE` | `NoStuckPending` intentionally VIOLATED: ListenRejects, Relistens, then the listener never resolves; 'Opening session' stays with no read and no action | 5,856 distinct states; 22,536 generated | 18 | <1s |
| `MCDesktopUpdateListenerRegistrationGap.cfg` | as green, `FullAfterListen = FALSE` | `ViewConverges` intentionally VIOLATED: the read without a listener takes version 1, a commit before registration is dropped, the listener resolves and nothing reads version 2 | 12,735 distinct states; 54,745 generated | 19 | <1s |
| `MCDesktopUpdateListenerListenFailure.cfg` | as green, `Relisten = FALSE` | `ViewConverges` intentionally VIOLATED: the read without a listener settles the session, registration rejects and is never retried, and the next commit is never read | 6,073 distinct states; 25,790 generated | 16 | <1s |
| `MCDesktopUpdateListenerPendingNoEvent.cfg` | as green, `PendingReread = FALSE` | `NoStuckPending` intentionally VIOLATED: a read answers a waiting phase (`accepted` in the trace), then a silent arrival; the waiting status stays with no action | 5,093 distinct states; 18,904 generated | 15 | <1s |
| `MCDesktopUpdateListenerSupersededMismatch.cfg` | as green, `MismatchExit = FALSE` | `NoStuckPending` intentionally VIOLATED: a reply for another session (FinishMismatch) is dropped and the load stays `loading` with no read behind it | 10,903 distinct states; 46,605 generated | 17 | <1s |
| `MCPackUxWatch.cfg` | all guards, all four hazards, `RetryBudget = 4`, `OutageAttempts = MaxRestarts = MaxFaults = MaxRev = 1`, `MaxSelects = 2` | Passes `TypeOK`, `NoCrossNodeDelivery`, `RegistryReleased`, `NotSilent`, `TerminalNamed`, `EventualResume`, `NoLeakedWatch`, `EntryReleased` | 380,360 distinct states; 1,781,565 generated | 34 | 29s (OpenJDK 25, 4 workers) |
| `MCPackUxWatchTransient.cfg` | `RetryTransient = FALSE`, no hazards, `MaxRestarts = MaxSelects = 0` | `EventualResume` intentionally VIOLATED: Start, Connect, Write, SendAtomic, ReadTimeout, Notify, Release ends a healthy runtime's watch | 139 distinct states; 311 generated | 10 | <1s |
| `MCPackUxWatchStaleEndpoint.cfg` | `Reacquire = FALSE`, no hazards, `MaxSelects = 0` | `EventualResume` intentionally VIOLATED: after Restart every Connect uses the old endpoint until the budget ends the watch | 2,487 distinct states; 6,550 generated | 23 | <1s |
| `MCPackUxWatchNodeSwitch.cfg` | `CancelOnSelect = FALSE`, no hazards | `NoCrossNodeDelivery` intentionally VIOLATED: Start, BeginSelect("B"), EndSelect, Connect, SendAtomic | 128 distinct states before counterexample; 221 generated | 6 | <1s |
| `MCPackUxWatchSelectBeforeCancel.cfg` | `CancelBeforeSelect = FALSE`, no hazards | `NoCrossNodeDelivery` intentionally VIOLATED: Start, BeginSelect("B") writes the selection, Connect, SendAtomic before the cancellation | 71 distinct states before counterexample; 106 generated | 5 | <1s |
| `MCPackUxWatchUnlockedDeliver.cfg` | `AtomicDeliver = FALSE`, no hazards | `NoCrossNodeDelivery` intentionally VIOLATED: Connect, Check, BeginSelect("B"), EndSelect, SendChecked | 454 distinct states before counterexample; 1,085 generated | 8 | <1s |
| `MCPackUxWatchNoDestroyCancel.cfg` | `CancelOnDestroy = FALSE`, no hazards | `RegistryReleased` intentionally VIOLATED: Start, Destroy | 30 distinct states before counterexample; 37 generated | 4 | <1s |
| `MCPackUxWatchSilentBackoff.cfg` | `NotifyStall = FALSE`, no hazards, `MaxSelects = 0` | `NotSilent` intentionally VIOLATED: Start, Restart, Connect backs off with nothing shown | 45 distinct states before counterexample; 67 generated | 5 | <1s |
| `MCPackUxWatchResetOnAccept.cfg` | `KeepFailuresOnAccept = FALSE`, hazard `broken`, `MaxRestarts = MaxFaults = MaxSelects = 0` | `EventualResume` intentionally VIOLATED: Connect, SendAtomic, StreamBroken, Notify, Retry cycles forever | 297 distinct states; 749 generated | 13 | <1s |
| `MCPackUxWatchUnbounded.cfg` | `BoundedRetry = FALSE`, hazard `stop`, `MaxRestarts = MaxFaults = MaxSelects = 0` | `EventualResume` intentionally VIOLATED: a stopped runtime is retried forever and never reported | 536 distinct states; 1,257 generated | 20 | <1s |

The nine BackgroundAdmissionContention rows used TLC2 `2026.11.06.222357`
(rev `d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and
`-workers 2`. The green run checked its temporal property over the complete
state space. The counts before each counterexample vary with the worker
count.

The six SessionHeadObservation rows used TLC2 `2026.11.06.222357` (rev
`d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and `-workers 2`; the
liveness rows checked `ReplyObserved` over the complete state space.

The ten RuntimeConnectionTransport rows used TLC2 `2026.11.06.222357` (rev
`d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and `-workers 2`; the
green rows checked their temporal properties over the complete state space.

The fourteen ObserverResync rows used TLC2 `2026.11.06.222357` (rev `d8fb231`)
with Homebrew OpenJDK 25.0.2 on macOS arm64 and `-workers 2`; the liveness
rows checked their temporal properties over the complete state space.

The seven DesktopUpdateListener rows used TLC2 `2026.11.06.222357` (rev
`d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and `-workers 2`; the
green runs checked their temporal properties over the complete state space.

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
