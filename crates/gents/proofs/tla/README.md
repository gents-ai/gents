# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `PairingTransport` — connection establishment + replication liveness for one directed pairing edge, the transport layer below the reconciler. The Lean `PairingReconcile` model assumes the transport carries the install RPC / that `connect` succeeds; it does not model *establishing* the link, so the live #511 fleet hang was outside the modeled world. This spec closes that gap. It distinguishes the **three real failure modes** (grounded in the production code) with two independent BOOLEAN constants — `Dialable` (the connect-gate ticket form) and `ReplicatorInstallable` (whether `add_replicator` can succeed once connected) — exercised by a **three-config diagnostic**: `MCPairingTransportDialable` (both true: the shareable-address fix — all properties hold); `MCPairingTransportUndialable` (MODE A — `Dialable = FALSE`: connect-fails-first, the *literal* #511 hang, never reaches `Connected` so nothing subscribes and no applied row is written); `MCPairingTransportReplicatorStuck` (MODE B/C — `ReplicatorInstallable = FALSE`: connect OK and collections subscribe but the replicator install never succeeds — the durable "subscribed collections, `replicator_addresses` null" partial row). Companion to the Lean `PairingReconcile` `dialFailed` (MODE A) and `reconcileInstallReplicatorFailed` (MODE B/C) transitions and the `convergence_requires_successful_install` obligation.
- `P2PBackpressure` — hub fan-in/fan-out admission **obligation** model for #630, not a multi-wave flood-safety proof. `PairingTransport` proves a single edge can connect/install; this model starts after that. Its one-wave necessity obligations say outbound timeouts release push-worker slots and inbound success acks are backed by modeled pending registration or merge. The pinned DefraDB implementation has since added bounded queue admission before worker execution, per-peer scheduling/coalescing, persisted retry handoff on overflow, and durable push-originated pending-DAG recovery. Those multi-wave and restart properties, plus Bitswap stalls, rate limits, gossip-loop health, multi-slow-peer fill, and peer-vs-CID pending, remain outside this model. Receiver-side pending-DAG slot starvation and acknowledged-DAG TTL loss belong to the defradb.rs receiver dispatch model (`proofs/tla/SyncOwnership.tla`, `MC_SyncOwnership_Dispatch` configs), not here. See `boundary.p2p-backpressure.obligation-model`. Green/red configs: `MCP2PBackpressureGreen` / `TimeoutStall` / `BadAck`.
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
- `ClientSubmissionContention` — a User's optimistic chat send (Lean
  `ClientOptimisticSend`) racing writes to the documents it reads. The client
  picks the request identity, shows the message pending, keeps its payload
  and sends; a reply confirms it, and a refusal or the client's deadline
  shows it failed with Retry, which resends under the same identity. Each
  send is the reservation (`request.client.reserve_session`) and the
  submission (`request.client.submit`), whose commits abort when the working
  Agent refreshes the session observation; both are signed-conflict rebuilds
  (Lean `clientSignedWriteMayRetry`). The runtime's create is idempotent on
  the request identity (`send_client_request`: the key derived from the
  identity, the held lookup by `request_id`, the staging held check) and the
  runtime refuses only an identity it does not hold; the transport may fail
  any send. `Timeout` is the client deadline `CHAT_SEND_DEADLINE_MS`.
  `NoConflictLoss`, `CreatedOnce`, `RetainedUntilConfirmed`,
  `FailedOffersRetry` (Retry is enabled for every failed message below the
  model's send bound), `PendingResolves` and `EverySendSettles` hold under
  lost replies, refusals, transport failures and retries racing an earlier
  send of the same identity. `SingleAttempt` is the found submission race
  (the busy-session mailbox test); `ReserveSingleAttempt` is the found reservation race ("a
  partial read lost to a newer merge" in the live backgrounding run);
  `ShortBudget` shows the overlap premise is load-bearing; `RetryFreshId`
  retries under a new identity and duplicates the turn; `NoPayload` drops the
  payload of an unconfirmed message and leaves a failed message with no
  Retry.
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
- `InterruptPartialOutput` — provider text the owned loop has received when
  its request is interrupted (#3820). `NoReceivedOutputDropped` and
  `TerminalOnce` hold when the loop yields a received chunk in the step that
  receives it. `SuspendedHandoff` is the found race: the handoff
  from the provider work to the loop's yield could return `Pending` on a
  spent task budget, and the biased interrupt branch drops the stream with
  the chunk in the reply. `NoDrainFlush` drops the drain's
  flush; `Witness` shows an interrupt after output keeps it.
- `GoalSourceObservation` — a test observing the goal owner, which runs only
  inside `next_fire` (#3820). A fixed timeout (`FixedWindow`) fails a correct
  owner; waiting for the transition passes it and still fails an owner that
  fires (`FiringOwner`).
- `RuntimeConnectionTransport` — the desktop observer's authenticated SSE
  connection to one runtime it reads as the User (`client/observe`). The
  source task arms `K` subscriptions one at a time and then sends a ready
  notice; every renewal period (weakly fair) it arms a replacement set while
  the current set keeps streaming, swaps them and sends `renewed`
  (make-before-break); a stream ends by a transport failure or a refusal,
  and the task ends, always with an end notice. The observer applies only
  notices of the current source generation, runs one snapshot at a time, and
  reconnects an ended source after the Lean `reconnectDelayMs` of its
  consecutive reconnect count (flat for `Flat` reconnects, then doubling to
  `Cap`); the count restarts only after the source stayed connected for the
  stable period (`Stabilize`, Lean `reconnectStableMs`), never at an arm or a
  snapshot alone. A snapshot that failed for a transport reason keeps the
  streams armed and is retried; it never reconnects. Reconcile replaces or
  drops the source when the peer's endpoint changes or the peer is removed,
  keeps a source whose task ended until its end notice schedules the
  reconnect, and an aborted task may still enqueue one notice. Nothing
  omitted or empty is shown as a refusal: native ACP hides what a reader may
  not read. `Revocation` is how the runtime answers a revoked User:
  `"served"` is the pinned served runtime (handshake accepted, streams
  silent, reads omit the selected roots; observe test
  `revoked_user_is_answered_with_omitted_roots`), read as an empty connected
  view; `"handshake"` and `"envelope"` are the access owner's refusals,
  which reconnect on the backoff and are shown as `refused` across every
  reconnect. `Drops` adds a flapping runtime that drops an open stream at any
  time. `ConnectedAfterArmedSnapshot`, `StaleNoticeInert`, `RemovalClears`,
  `NoEndpointNamed`, `RenewalNeverDisconnects`, `NoCauselessReconnect` (a
  quiet or empty view is never a reason to reconnect), `ReArmedWithinCap` (a
  runtime that answers and accepts the client is re-armed within `Cap` ticks
  of doing so, however many reconnects came before), `FlapBounded` (at most
  `Flat` consecutive reconnects at the base delay without a stable
  connection), `ReconnectsWaitTheBackoff`, `EventuallyConnected`,
  `RecoveredEventuallyArmed`, `RefusalNamed` and `NoEndpointEventuallyNamed`
  hold for all three (GREEN `Transport`, `TransportEnvelope`,
  `TransportHandshake`, with `Flat = 1`, `Cap = 4`: two doublings before the
  cap); GREEN `Flap` checks the safety properties against a flapping runtime.
  RED: `OmittedRoot` and `OmittedRootLiveness` (an omitted root read as a
  failure reconnects a revoked User's quiet runtime forever, the retry
  storm), `UncappedBackoff` (without a cap, a runtime that recovers while the
  source waits 8 ticks is re-armed more than `Cap` ticks later),
  `ResetOnConnect` (a count reset by a connected snapshot redials a flapping
  runtime at the base delay forever), `EarlyRestart` (a reconcile restarts an
  ended source before its notice and skips the backoff), `BreakBeforeMake` (a
  renewal that drops the armed set shows Connected with nothing armed),
  `RenewalFlap` (a renewal reported as a transport failure), `NoEndpoint`,
  `StaleGeneration` and `EarlyReady`. Lean `renewalStartMs` spaces renewals a
  full period after each completed arm.
- `WindowIndependence` — several desktop windows, each on its own node
  (Lean `ClientWindowScope`). Each node's observer source publishes into that
  node's own store, whose change notice is broadcast to every window; each
  window owns its selection, pack watch and view, reads its own node's store
  and publishes only within its view epoch. `NoCrossDelivery` (a window shows
  and is delivered only its own node), `Converges` (each window converges for
  its own node) and `WatchFollowsSelection` hold (GREEN
  `MCWindowIndependence`, two windows on two nodes). RED: `NoCancelOnSwitch`
  (a selection change keeps the old node's watch) breaks `NoCrossDelivery`.
- `ObserverResync` — the desktop observer's delivery and resync loop for
  runtime documents and the direct refresh beside it, with one store per
  node. A runtime write emits a subscription event only after the stream is
  armed; the observer queues the node, starts one load that captures the
  node's store fence, reads and publishes through the fence; a direct refresh
  does the same once per user mutation. `NoLostWrite`, `NoStaleCommit`,
  `NoReadWithoutReason`, `EventualPublication` and `Convergence` hold when
  request events stay off the reload fence, a rejected load requeues without
  back-off and a rejected refresh hands its node to the observer (Lean
  `ClientDataPublication.acceptsReload`, `ObserverResync`). The reload fence
  is the generated table of `ClientDataPublication.reloadAcceptedSince`.
  `NoReadWithoutReason` bounds hosted reads by batches of reasons to read, not
  below them: with a lease renewal during every load the node is read back to
  back. `RejectDelay` restores the 250 ms wait after a rejected load and still
  passes: the delay is a latency choice, not a guard. `SustainedEvents` is the
  found livelock: one lease renewal during every load, with request events
  moving the fence every reload checks, rejects every load forever and the
  node stays connecting with stale rows. `ReconnectGap` keeps the code before
  the fix without renewals as a regression guard for arm-then-snapshot.
  `NodeScopeOnly`, `NoPublicationFence`, `NoEventQueue` and
  `SnapshotBeforeArm` each break one remaining guard.
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
- `NativeCompletionHandshake` — the scheduling handshake around the embedded
  transaction owner's caller-timeout step (Lean `NativeCompletion`
  `callerTimedOut`), which fixes the event order but not which party may act
  when. The caller takes its native permit, spawns the native task and arms
  the storage-step deadline in one uninterrupted step, the native operation
  reports entry and parks, and a driver pauses the virtual clock and advances
  it to expire that deadline; the expiry abandons the tracker and a telemetry
  record may follow. The model has no real-time step, so a behavior that never
  observes the timeout is one a driver can only finish by waiting on wall
  time. `AdvanceFollowsArm`, `ObservationImpliesAbandonment` and
  `ObservableWithoutRealTime` hold when the driver advances only after the
  native entry it waited for and observes the tracker's own abandonment under
  notify-permit semantics. `UnarmedAdvance` drops the entry precondition and
  advances under the deadline the caller later computes; `TelemetrySignal`
  waits on the `pending_native` write-attempt event, a projection the attempt
  may skip once it holds a terminal outcome; `LostWakeup` drops permit
  semantics, the lost wakeup an enable-then-check wait avoids.
- `GuestAllowance` — a computing plugin guest that never calls the host and
  is stopped only by its compute allowance (Lean `allowanceTicks`), an epoch
  deadline advanced by the WASM host's ticker thread. `MCGuestAllowance`
  (the ticker advances the epoch by elapsed periods) holds;
  `MCGuestAllowanceWakeCounted` (the shipped ticker, one increment per wake,
  every wake late) fails `StopsAtAllowance` and `WakeCountedNotLost` shows
  that stop is late, not lost; `Punctual` holds with on-time wakes;
  `Unchecked` (no epoch checks in loops) never stops the guest;
  `CancelLate` shows a cancel can find the guest already stopped.
- `Sanity` — toolchain smoke test; not a real model.

## Checking

Each model config's first line states the result TLC must report for it:

```
\* expect: holds
\* expect: deadlock
\* expect: violates <Name>
\* expect: violates <Name> initially
```

`<Name>` is one `INVARIANT` or `PROPERTY` the config checks; `initially`
records that the initial state violates it, and a violation in the initial
state matches no other header. `deadlock` requires deadlock checking on. TLC
must also report at least two distinct states, or the number an optional
second line `\* states: <N>` records, so a model whose initial predicate
admits nothing, or that stops after its first state, does not pass. Each spec follows its `---- MODULE X ----` line
with the Lean decisions it models and the crates that implement it:

```
\* lean: none | <Lean.owner>, <Lean.owner>
\* crates: none | <crate>, <crate>
```

An owner is a Lean declaration exported to the conformance snapshot (its key
in `crates/gents-lean-vocab/families.schema.json`); `none` states that the
spec models no Lean decision. A crate is a workspace package. These headers
are the record of expected results; `scripts/ci/test_gate.py` checks them.

`scripts/tla_check.py run --all` model-checks every config in breadth-first
mode with one worker per config and a fixed fingerprint, compares each result
with its header and prints a table; a mismatch, a missing header, a TLC error
or a timeout (`--timeout`, seconds per config) fails it. `--spec`, `--config`,
`--crate`, `--lean` and `--files` narrow the run. The pull request gate runs
the specs whose headers name a changed crate or Lean owner, or whose own files
or generated imports changed; main and the nightly `TLA+ nightly` workflow run
every config. `scripts/tla_check.py install` installs the pinned JDK and
`tla2tools.jar` (version and SHA-256 in the script) under
`~/.cache/gents-tla`. `scripts/run-tlc.sh <MC config>` runs TLC with them and
prints its full output, counterexample included.

## Generated Lean decisions

`make lean-regen` writes `generated/LeanDecision_<Namespace>.tla` for every
owner a spec names that Lean exports as a `conformance_function` over a finite
domain: `<name>_table`, the decision over its whole domain keyed by the tuple
of its arguments in Lean parameter order and written as a balanced tree of
`@@` (a long left-nested chain overflows TLC's stack), and `<name>(...)`, its
lookup. A
spec `EXTENDS` that module instead of restating the decision, and must not
define an operator of the same name. `generated_tla_modules_are_current` in
`gents-lean-vocab` fails when a module is stale. The scripts put `generated/`
on TLC's library path (`-DTLA-Library`).

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

Active lines from `MCP2PBackpressureGreen.cfg` (`TimeoutReleasesSlot = TRUE`, `AckWithoutPendingAllowed = FALSE`):

- **`INVARIANT TypeOK`** — outbound states and inbound admission sets stay in their declared bounded domains.
- **`INVARIANT PushSlotsBounded`** — in-flight outbound PushLog sends never exceed `PushWorkers`.
- **`INVARIANT PendingBounded`** — registered inbound pending DAGs never exceed `MaxPending`.
- **`INVARIANT SuccessAckBacked`** — every inbound success ack is backed by either a merged block or a pending-DAG registration; overflow must nack.
- **`INVARIANT FailedOnlyUnresponsive`** — healthy peers cannot be marked failed in the current push wave.
- **`PROPERTY HealthyPeersDeliver`** — every responsive peer eventually reaches `Delivered`, even if the nonresponsive peer gets the first worker slot.
- **`PROPERTY InboundSettles`** — every inbound PushLog eventually receives either success or nack.

The diagnostics each reproduce one load-bearing failure:

- **`MCP2PBackpressureTimeoutStall.cfg`** (`TimeoutReleasesSlot = FALSE`) — intended counterexample: start `slow` first, fill the only worker slot, leave healthy peers queued forever so `HealthyPeersDeliver` fails. This encodes **necessity** of slot release in a one-worker toy: if timeout does not free the semaphore, healthy delivery can fail even under fairness. It is **not** a sufficiency claim that Amy hub saturation is solved when the green BOOLEAN holds (production can still fail via multi-slow-peer fill, re-queue storms, Bitswap stalls, or gossip-loop death after the permit is released).
- **`MCP2PBackpressureBadAck.cfg`** (`AckWithoutPendingAllowed = TRUE`) — intended counterexample: fill the pending-DAG map and success-ack a second inbound PushLog without merging or registering it so `SuccessAckBacked` fails. Formal counterpart of the production invariant (defradb.rs #1089 / pinned hub admission) that pending-DAG capacity overflow must return the backpressure nack, not success.

## Fairness annotations


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
