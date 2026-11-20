# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `PairingTransport` — connection establishment + replication liveness for one directed pairing edge, the transport layer below the reconciler. The Lean `PairingReconcile` model assumes the transport carries the install RPC / that `connect` succeeds; it does not model *establishing* the link, so the live #511 fleet hang was outside the modeled world. This spec closes that gap. It distinguishes the **three real failure modes** (grounded in the production code) with two independent BOOLEAN constants — `Dialable` (the connect-gate ticket form) and `ReplicatorInstallable` (whether `add_replicator` can succeed once connected) — exercised by a **three-config diagnostic**: `MCPairingTransportDialable` (both true: the shareable-address fix — all properties hold); `MCPairingTransportUndialable` (MODE A — `Dialable = FALSE`: connect-fails-first, the *literal* #511 hang, never reaches `Connected` so nothing subscribes and no applied row is written); `MCPairingTransportReplicatorStuck` (MODE B/C — `ReplicatorInstallable = FALSE`: connect OK and collections subscribe but the replicator install never succeeds — the durable "subscribed collections, `replicator_addresses` null" partial row). Companion to the Lean `PairingReconcile` `dialFailed` (MODE A) and `reconcileInstallReplicatorFailed` (MODE B/C) transitions and the `convergence_requires_successful_install` obligation.
- `P2PBackpressure` — hub fan-in/fan-out admission **obligation** model for #630, not a multi-wave flood-safety proof. `PairingTransport` proves a single edge can connect/install; this model starts after that. Its one-wave necessity obligations say outbound timeouts release push-worker slots and inbound success acks are backed by modeled pending registration or merge. The pinned DefraDB implementation has since added bounded queue admission before worker execution, per-peer scheduling/coalescing, persisted retry handoff on overflow, and durable push-originated pending-DAG recovery. Those multi-wave and restart properties, plus Bitswap stalls, rate limits, gossip-loop health, multi-slow-peer fill, and peer-vs-CID pending, remain outside this model. Receiver-side pending-DAG slot starvation and acknowledged-DAG TTL loss belong to the defradb.rs receiver dispatch model (`proofs/tla/SyncOwnership.tla`, `MC_SyncOwnership_Dispatch` configs), not here. See `boundary.p2p-backpressure.obligation-model`. Green/red configs: `MCP2PBackpressureGreen` / `TimeoutStall` / `BadAck`.
- `P2PBackpressure` — hub fan-in/fan-out admission **obligation** model for #630, not a multi-wave flood-safety proof. `PairingTransport` proves a single edge can connect/install; this model starts after that. Its one-wave necessity obligations say outbound timeouts release push-worker slots and inbound success acks are backed by modeled pending registration or merge. The pinned DefraDB implementation has since added bounded queue admission before worker execution, per-peer scheduling/coalescing, persisted retry handoff on overflow, and durable push-originated pending-DAG recovery. Those multi-wave and restart properties, plus Bitswap stalls, rate limits, gossip-loop health, multi-slow-peer fill, and peer-vs-CID pending, remain outside this model. Receiver-side pending-DAG slot starvation and acknowledged-DAG TTL loss belong to the defradb.rs receiver dispatch model (`proofs/tla/SyncOwnership.tla`, `MC_SyncOwnership_Dispatch` configs), not here. See `boundary.p2p-backpressure.obligation-model`. Green/red configs (`MCP2PBackpressureGreen` / `TimeoutStall` / `BadAck`) are **modeled, not yet TLC-checked**.
- `PairingGrantRevocation` — an application pairing racing the withdrawal of
  its author's P2P mutation grant. The reconciler reads authority when it
  prepares a peer and applies the owned diff one transport operation at a
  time; a scope change is a teardown then an install; a replicator pushes and
  backfills with no grant check. The green config (each install re-derives
  its scope from authority read after the operations before it; the observer
  waits for the base route to stand without the collection) keeps
  `NoStaleInstall`, `ObservedWithdrawalHolds` and `WithdrawalTakesEffect`.
  `NoRecheck`/`NoRecheckObserved` are the found race (the CI failure trace);
  `GapObserver` acts inside the reinstall gap. `Strict` shows that no
  Gents-side ordering keeps every document written after the withdrawal off
  the peer, and `RevokeTearsDown` that a revoke-driven teardown does not
  either; only `PushChecksGrant`, a push-time grant check in the transport,
  does.
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
- `LeaseRenewalStarvation` — one request's execution lease, its owner and
  its renewal task on the owner's runtime, beside the wall clock (#3704).
  The claim starts the lease and the renewal task together; the renewal
  polls every quarter lease and renews once half a lease remains; the
  publication fence admits output only from the owning generation before
  its deadline (Lean `RequestExecutionLease`). The owner also runs one
  synchronous step that holds its runtime thread, the first Lean snapshot
  read of a test. `SingleWriter` holds whatever the owner's runtime does
  (`Fenced`); `OwnerPublishes` and `LeaseHeld` hold when the step runs
  before the claim (green), off the runtime (`OffRuntime`) or for less than
  half a lease (`BoundedSync`). RED: `SyncInsideLease` is the failing test's
  order and breaks `OwnerPublishes`; `MarginSpent` spends half a lease and
  breaks `LeaseHeld`; `Unfenced` drops the deadline check and breaks
  `SingleWriter`.
- `RetryBackoffLease` — one request's execution lease while its owner backs
  off a failed provider call, beside the drive's lease poll, the renewal task,
  recovery and the e2e fixture that expires the lease. The wake decision is
  Lean `RequestExecutionLease.retryWakeDispatches`. `DispatchOnlyLive` holds
  only when the wake is authorized against the lease (`Fenced`); `Unfenced`
  is the product before the fix, where an expiry after the drive's last poll
  and before the wake dispatches the retry under an expired lease. The
  fixture's observations hold with an unreached backoff and a
  generation-only expiry write (green); `WallClock` (a backoff shorter than a
  slow fixture) and `DeadlineCas` (a renewal between the fixture's read and
  write) are the test's accidents.
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
- `InterruptLatchContention` — a User's interrupt of a running turn
  (`interrupt.latch_request`, the desktop's and CLI's request interrupt)
  racing the turn's own writes to the `AgentRequest` document: the execution
  lease renewal and any other interrupter. The latch reads the row and sends
  a User-signed update stamping `interrupt_requested_at` unless it is already
  set; its commit aborts when a write of the row lands after the read. It is a
  signed-conflict rebuild (Lean `TransactionReplay.disposition`) whose every
  attempt reads the row afresh. `NoConflictLoss`, `StampedOnce`,
  `DoneIsLatched` and `InterruptSettles` hold while fewer writes land in one
  attempt series than it has attempts (the renewal cadence premise).
  `SingleAttempt` is the found race (`transact_write` never rebuilds a sent
  signed write, so one renewal failed the interrupt with a DefraDB
  transaction conflict); `StaleRebuild` keeps the first read and stamps a
  latch another interrupter set; `Exhausted` drops the cadence premise.
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
- `WallClockBound` — a wall-clock bound racing an owner's progress in a test
  (#3840): an observer that judges at a fixed window (a timeout, a sleep
  before one read, an elapsed-time bound, a fixture that stops after a delay;
  `FixedWindow`) or a fixture deadline the product enforces (`WallDeadline`)
  fails a correct owner on a slow host. Waiting on the owner's notification
  under the suite's stall ceiling, with the fixture deadline past any run or
  driven by the test after the steps it must follow (`Driven`), passes under
  every schedule and still fails an owner that never commits
  (`StalledOwner`); `SlowHost` is the ceiling's premise.
- `NativeWorkerRecordObservation` — a test waiting until two background
  native workers run and have recorded their process ownership. Each
  `spawn_process` handler commits its spawned row running and then the
  worker's task spawns the child and renames its record into the runtime's
  record directory, a host file no document commit follows. Green subscribes
  to `AgentToolCall` and watches the record directory before the request
  exists, re-reads after each notification and fails only at the stall
  ceiling. RED: `FixedWindow` (the CI failure: a 20 s timeout fails a
  progressing owner on a loaded host), `DocumentsOnly` (the last record lands
  after the last commit and the waiter fails a finished owner),
  `SubscribeAfterRead` (a change between a short read and the subscription
  is never signalled); `StalledOwner` shows the waiter still fails an owner
  that never records.
- `ToolDeadlineInheritance` — where an accepted tool call's deadline comes
  from. The claim fixes the request's one deadline; the provider-turn
  publication writes it on the `AgentToolCall` row and the session hook's
  bound deadline becomes the dispatched lifecycle's. Bound from the claim, as
  the runtime binds it, the two agree under every schedule (Lean
  `coherent_tool_deadline_eq_request_deadline`). A fixture that binds the
  hook from its own clock reading gives one request two deadlines:
  `ShortClaim` (before #3840) passed the test only by their ratio and failed
  on a slow host; `LongClaim` (after #3840) fails it on every run.
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
- `ObserverCollectionReload` — the observer's collection reloads beside its
  full node reloads (Lean `ObserverResync.deliver`, `collectionStartable`,
  `startCollections`, `settleCollections`). A change to a collection whose
  snapshot root is self-contained queues that collection's own reload; any
  other change queues the node's full reload. One read per node is in flight,
  full reloads first, and a full reload takes the queued collections with it.
  A collection reload publishes only its collection through the reload fence
  (the generated table of `ClientDataPublication.reloadAcceptedSince`); a
  rejected one queues its collections again and a failed one queues the full
  reload. `NoLostWrite`, `NoStaleCommit`, `EventualPublication` and
  `Convergence` hold with a touch (the per-minute observation rewrite) and a
  lease renewal during every full load, a stream drop, a direct refresh and a
  failed collection read (GREEN `MCObserverCollectionReload`). RED:
  `IgnoreFence` (a collection reload publishes through a moved fence) breaks
  `NoStaleCommit`; `DropRejected` (a rejected collection reload drops its
  collections) and `DropFailure` (a failed collection read queues nothing)
  break `NoLostWrite`; `ConcurrentReads` (a collection reload may run beside a
  full reload) breaks `EventualPublication`: every touch rejects the full
  reload in flight.
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
- `StoreHandoff` — handing one local store (a client installation's, the
  desktop's or a runtime's home) from the process that holds it to the next
  process that opens it. The store is excluded by an OS advisory lock that
  the kernel drops when its holder exits; the lock file carries the holder's
  pid and, once it begins to stop, its declaration that it is releasing. The
  holder serves, may begin to stop, releases and acknowledges the stop,
  retire or quit; it may crash or wedge. Each acquirer tries the lock once,
  reads the declaration of a held lock and waits (bounded by its own
  deadline) or is refused naming the holder (Lean `StoreHandoff.decide`; the
  holder's order is Lean `ClientInstallation.step`). `NeverTwoHolders`,
  `AckFollowsRelease`, `AckedHandoff`, `RefusalNamesLiveHolder`,
  `CrashedHolderNeverBlocks`, `AcquirerSettles`, `WaiterTakesReleasedStore`
  and `StopAcknowledged` hold in the green model. RED: `Current` is the code
  before the fix (retire answered on acceptance, nothing declared, every
  held lock refused) and breaks `AckedHandoff`; `AckBeforeRelease` breaks
  `AckFollowsRelease`; `RefuseStopping` breaks `RefusalNamesLiveHolder`;
  `UnboundedWait` breaks `AcquirerSettles`; `PidFile` breaks
  `CrashedHolderNeverBlocks`.
- `SharedOwnerHotCounter` — concurrent read-modify-write increments of one
  document through the shared transaction owner (`transact_write` in
  `ConflictRetry` mode on an embedded node), the
  `shared_owner_hot_counter_has_no_lost_updates_at_c64` workload. Writers
  take a mutation-gate permit (a FIFO semaphore that parks and wakes), open a
  native transaction, read the counter in it, and commit under Regolith's
  validation of every read and written key against its observed sequence; a
  conflict repeats the callback on a fresh transaction while Lean
  `TransactionReplay.disposition` allows, and a commit-phase timeout is a
  storage fault that is never repeated. `AckedApplied`, `ConflictNotApplied`,
  `NoDoubleApply`, `NoLostUpdate`, `TestAccounting`, `AttemptBound`,
  `PermitsAccounted` and `EveryCallSettles` hold with the gate contended and
  commits that may time out; `Unbounded` shows first-committer-wins alone
  rules out livelock. `ReadOutsideTxn`, `StaleRetry`, `ConflictAsSuccess` and
  `RetryAmbiguous` each break one safety guard; `LostWakeup` and
  `AbortOnOpenWriter` are the two stall shapes, each refuted for the code.
  `WaveDeadline` is the test's 60 s per-wave timeout as a property: wall time
  passes while the writers wait for the host, so it fails with every safety
  property holding. `Native` is one round of raw single-attempt transactions
  without the owner, as the native c64 experiment runs them: `RoundProgress`
  (some writer commits) holds by first-committer-wins, and
  `NativeAbortOnOpenWriter` (overlapping writers aborted together) ends a
  round with no commit.

- `DroppedPermitCleanup` — one admitted provider call's `InferenceCall` row
  from its creation to its terminal, and what a reader may await before
  reading it (#3659). The holder finishes explicitly (the terminal write is
  awaited) or its permit or queued guard is dropped, and `Drop` hands the
  terminal write to a detached cleanup the registry's tracker counts. Every
  terminal is a compare-and-set on a live row. The runtime stops gracefully
  (join the tasks that hold permits, await the tracker, exit), is cut by the
  teardown deadline, or exits abruptly; an exit discards cleanup in flight,
  and recovery terminalizes a live row whose call ended, with its own reason
  and no usage. `ExactlyOneTerminal`, `ObserverSawTarget`,
  `GracefulExitFlushed` and `EndedCallTerminal` hold in the green model, whose
  reader awaits the flush after ending the permits; GREEN `Changes` and
  `ChangesTerminal` add `ObserverSettles` for a reader on the store's change
  subscription armed before its first read, the signal for a row no drop of
  the reader's own owes (a spawned call's queued row). RED: `UntrackedSpawn`
  is the code before #3692; `UnawaitedFlush` is a graceful stop that does not
  await the tracker and discards a dropped permit's terminal and usage;
  `UncountedFinalizer` awaits it but does not count a stream's finalizer,
  which the stop aborts without joining, so the stop exits before that
  permit is dropped; `NoRecovery` leaves a live row after an abrupt exit; `NoTerminalCas` writes
  a second terminal over a finish dropped after its commit; `SleepTerminal`
  and `SleepQueued` are the timer waits; `QueuedByFlush` awaits the tracker
  for a row it does not count; `PollBudget` polls within a budget;
  `LateSubscribe` subscribes after the first read. Lean
  `InferenceCall.DroppedPermit` owns the terminal a drop records.
- `ClientIdentityMetadata` — first-run writers of one home's client identity
  metadata (#3659, the concurrent eval pilots). Each writer reads the
  metadata, stages it beside the destination, renames it over the
  destination and reads again; what it does with each read is Lean
  `ClientIdentityMetadata.decide`. `NoMissingFileError`, `NoTornMetadata`,
  `NeverUninstalled`, `OkSawOwnMetadata`, `AgreedInstall`, `WriterSettles`
  and `SameKeyAccepts` hold with a staging name per writer, one rename, the
  second read and a single-winner signing key, with writers dying at any
  step. RED: `Before` is the code before #3692 (the loser's rename finds
  `principal.tmp` gone); `SharedStaging` and `SharedStagingTorn` keep only
  the fixed staging name; `RemoveThenRename` and `RemoveThenRenameReread`
  keep only the removal; `NoReread` and `DivergentKey` drop the key premise,
  which is what keeps writers of one home in agreement.
- `GuestAllowance` — a computing plugin guest that never calls the host and
  is stopped only by its compute allowance (Lean `allowanceTicks`), an epoch
  deadline advanced by the WASM host's ticker thread. `MCGuestAllowance`
  (the ticker advances the epoch by elapsed periods) holds;
  `MCGuestAllowanceWakeCounted` (the shipped ticker, one increment per wake,
  every wake late) fails `StopsAtAllowance` and `WakeCountedNotLost` shows
  that stop is late, not lost; `Punctual` holds with on-time wakes;
  `Unchecked` (no epoch checks in loops) never stops the guest;
  `CancelLate` shows a cancel can find the guest already stopped.
- `GoalWrapup` — the one wrap-up turn of a Goal whose token budget is
  exhausted (#3810). Goal sources reconcile from the rows they read and
  write in separate transactions (begin the wrap-up, claim the parent,
  publish the child keyed by goal, parent and sequence, set the
  `wrapup_completed` latch); a caller that stops polling drops a step
  between them. `OneWrapupPerParent`, `NeverAfterCompletedWrapup`,
  `OneWrapup` and `LatchRecorded` hold for two sources. `Window` is the test
  that read the Goal after a fixed polling window and asserted the latch the
  source had not yet written; `NoWrapupGuard` drops the `requestIsWrapup`
  input of Lean `Goals.decide` and publishes a second wrap-up while the latch
  is unwritten; `UnkeyedChild` publishes two children of one claim.
- `BackgroundWakeIdentity` — concurrent background-completion notifications
  of one finished request coalescing into one wake request (#3810), and the
  fixture that prepares their tools. Under the process gate and DefraDB's
  read-conflict abort, `OneWake`, `SameIdentity`, `NoneFailed` and
  `EveryNotifierAnswered` hold. `WallClock` is the fixture that appended
  each tool's output observed at the wall clock and was refused by the claim
  deadline before any notification ran; `NoGate` loses a notifier to aborted
  attempts; `NoGateNoConflict` creates two wakes.
- `BackgroundEnvelopeProjection` — a session transcript where two background
  completions, their coalesced wake request and a person's look-alike inputs
  (the wake text and a quoted envelope) arrive in any order
  (`live_background_completions_project_as_tool_attributed_activity`). The
  worker claims in arrival order and publishes each input with its claim or
  fold; the projection classifies by Lean
  `TranscriptInputOrigin.classification` over the authoring request's facts.
  The observer stops once the notifications and every input, the wake's and
  each submitted one, are published; then `TestPasses` (the person's inputs
  render as a person's, the wake as a wake, the notifications as results)
  holds. `NoInputWait` is the observer before the fix: jobs that finish
  before the quote is submitted put the wake ahead of it, and the observer
  stops with the quote still pending. `ByText` classifies by the input's
  text and fails even the waiting observer.
- `StreamDelivery` — streamed provider text reaching a subscribed client
  while the provider pauses (#2131, #3803): the provider's chunks and its
  gated completion, the owned drive loop and stream writer (flush on the
  first visible bytes, on a due write, or at the batch deadline the drive
  waits on; Lean `StreamBatch.plan`), the segment append, the app store's
  request row and the client reader that joins both. `NoWithheldDuringPause`,
  `ChunksReachClient`, `ObservedBeforeCompletion` and `NoSpuriousFailure`
  hold for the paced and unpaced cases. `NoFlushTimer` flushes only on the
  next chunk or completion and `NoAppendOnDue` ignores the writer's due
  verdict; both strand a chunk during the pause. `RuntimeWakeOnly` is the
  test reader's lost wake-up and `Budget` its wall-clock visibility budget:
  the product is the green one in both. TLC2 `2026.11.06.222357` on Linux
  amd64, Temurin 17, `-workers 4`: green 214 and 72 distinct states, each red
  configuration violates its named property.
- `BackgroundLiveOutputObservation` — a background process observed mid-run
  (`list_background_tools_reports_running_live_output_bytes`): a gated child
  prints "live" and waits; the test polls `list_processes` until the row is
  running with 4 bytes, reads the same output with `read_process`, releases
  the gate and waits. List and read report the output at the moment they run,
  so `ReadAgrees` depends only on the gate still being closed when the read
  runs. `Timed` is the code before the fix: the child's own bounded wait (or
  its tool timeout) opens the gate after some ticks and the read returns 8
  bytes, the observed failure; `TimeoutKill` is the tool timeout killing it
  at the gate, which the wait reports as failed; `PollBudget` is the test's poll budget failing
  before the child prints. Green has neither clock.
- `ScriptFixtureExec` — a test that executes a script beside sibling tests
  that spawn (`supplied_path_retains_native_script_fallback`). A sibling's
  spawn copies the process's descriptors into its child until that child
  execs, and Linux execve refuses a file open for writing (ETXTBSY). RED:
  `InProcessWrite` writes the script in the test process, the failure on the
  Linux runner, and also what O_CLOEXEC alone gives (std already opens with
  it). Green never opens it for writing there (a checked-in fixture);
  `ChildWriter` writes a generated script from a child process the test
  reaps before its exec (`write_executable_script`), and its RED
  `ChildWriterUnwaited` drops the reap. `Darwin` (no such execve check) and
  `Alone` (no sibling) explain why the old order passed on macOS and alone.
- `TestPortReservation` — two gents-cli integration tests on one host, each
  in its own nextest process, reserving a port with `allocate_port`,
  starting a server on it and waiting with `wait_for_port`, which accepts any
  listener. `NoCrossTalk`: the server answering on a test's port is its own.
  RED: `PerRunner` locks reservations where each CI runner has its own
  view (TMPDIR, or `/tmp` under the runners' `PrivateTmp=yes`), so two jobs on one machine reserve the same port and one test talks
  to the other's server (the subscription EOFs and foreign-node errors on
  the shared Linux runners); `EphemeralOverlap` lets an outbound connection
  take the reserved port, which fails the server's bind. Green reserves with an
  abstract Unix socket, in the network namespace the ports share, below
  every ephemeral range; `PerRunnerSpread` shows
  why separate directories rarely collided while one process per binary
  spread its cursor.
- `ExecutableStaging` — the service updater stages an executable that is
  exec'd afterwards (`copy_executable`), while other threads spawn. A
  sibling's spawn copies the updater's descriptors until that child execs,
  and Linux execve refuses a file open for writing (ETXTBSY); a later exec by
  the service manager is not ordered after the siblings' execs. RED:
  `InProcessCopy` (`fs::copy` in the updater), `TempHandle` (a
  `NamedTempFile` held open until persist), `Unwaited` (the copy child not
  reaped). Green copies with a reaped `/bin/cp` child into a file the updater
  never opens for writing.
- `UsageRecordObservation` — a provider response's usage headers reach the
  runtime's `UsageCache` through a detached writer (`UsageReporter::observe`
  spawns credential resolution and the record), while the caller backfills
  the credential's `provider_account_key` between two responses and then
  reads the backend's usage. The binding is the row's stable `_docID`, so the
  backfill strands no observation; the reader is the side that can be wrong.
  Green subscribes to the cache's record count before checking and reads once
  both writers have recorded. RED: `ReadOnce` (the 2026-11-19 CI failure: the
  read is ordered after neither write and finds nothing), `SubscribeAfterRead`
  (a record landing between a short check and the subscription is never
  signalled).
- `CommitResponseWait` — a client's wait for a store request's answer over
  the runtime's HTTP endpoint on a loaded host (`txn_commit`, e.g. the pack
  document install; auto-commit mutations and reads are the same wait).
  RED: `ClientWindow`, a total or read-idle timeout, reports a live server's
  commit as unconfirmed (the change-unit CI failure); `NoKeepalive` waits
  forever on a connection whose host vanished; `Wedged` drops the premise
  that the server answers every commit it works on; `DroppedAnswer` reports a
  live server's outcome unknown after a broken connection. Green waits for
  the answer, a reset or a failed keepalive. `WedgedProgress` (a published
  commit step) and `DroppedAnswerQueried` (a transaction outcome query) are
  green designs DefraDB does not offer yet.
- `OfflineAccessRouting` — a CLI command against a home another process
  holds: a server claims the store, binds its port and publishes
  `runtime.json` only once ready; the command routes to a published runtime
  and otherwise tries the store lock, which `StoreHandoff.decide` refuses
  while the holder serves. `AssertionHolds`: the outcome is the one the test
  expects. Green waits for the publication and expects the server's answer,
  or holds the store itself and expects the refusal. RED: `PortWait` and
  `PortWaitAnswered` wait only for the port, where both outcomes are
  reachable (the `cli_session` fork test on CI).
- `ScheduleDisableFence` — a User disabling a schedule Trigger while the
  sequential schedule source fires it. Dispatch re-borrows the active
  snapshot and admits only an enabled Trigger; the router acknowledges only
  a generation it borrowed from the watch. `AdmissionFence` and `NoDueFire`
  hold: nothing admits the Trigger once its disabling generation is
  acknowledged, and a cursor written after that never fires. A fire admitted
  under the old generation can still commit after the acknowledgement.
  `OldObserver` is the CI failure: the test disabled while the interval
  cadence's seeded fire was in flight and counted its commit. Green waits for
  the seeded fire before disabling and judges once a witness Trigger, due
  after the disabled one, fires (`Settled`). RED: `NoSeedWait`, `NoWitness`
  (observer), `NoRecheck`, `AckBeforeWatch` (product).
- `HeldAudienceReads` — concurrent reads of a home's signed status by the
  clones of one endpoint, which share one held audience, clock and client
  authentication (#3963). A read starts after a credential refusal, before a
  request-bound delegation or a dated write, or to discover the runtime's
  client authentication; the home restarts between reads and a reader may be
  dropped mid-read. Green holds the cell's status lock from the read's nonce
  until what it read is kept and mints from the cell: `AudienceNeverOlder`,
  `ClockNeverOlder`, `MintsNewest`, `ReadsDoNotOverlap` and
  `EveryReaderMints` hold. RED: `Unordered` (the code before #3963: an
  earlier read replaces a later deployment), `Clock` (the same with no
  restart, regressing the clock and authentication), `Overlap` (two
  recoveries read and keep at once) and `OwnRead` (ordered reads, but a
  request mints from its own read's answer after a newer one was kept).
- `CallerAncestry` — a `gents runtime service stop` caller walking its parent
  chain to decide whether the service's job is its ancestor while processes
  exit, orphans are adopted and freed process IDs are reused. The caller asks
  the service manager for the job's pid, reads its own parent, then one
  parent per step; Lean `ServiceLifecycle.CallerObservation.callerRunsOnTarget`
  and `admission` decide. `NoSelfStop` (no admitted stop ends the job the
  caller runs under), `QuietOutsiderIsAdmitted` (an outside caller whose
  readable chain holds still is admitted) and `Terminates` hold. RED:
  `UnreadableAsRoot` ends the walk at an unreadable link as if at the root;
  `Subreaper` lets the job adopt orphans of its tree, which the Lean premise
  excludes, and a parent read through a reused pid misses the job;
  `ShortBound` gives up before the root of a real chain; `Unbounded` walks
  forever through reused pids.
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
admits nothing, or that stops after its first state, does not pass.

Each spec follows its `---- MODULE X ----` line with the Lean decisions it
reads and the crates that implement it, then optionally the Lean it restates
by hand and the code outside any crate that it models:

```
\* lean: none | <Lean.owner>, <Lean.owner>
\* crates: none | <crate>, <crate>
\* models: <Lean.declaration>, <Lean.declaration>
\* paths: <repository path>, <repository path>
```

A `lean:` owner is a Lean `conformance_function` over a finite domain (its key
in `crates/gents-lean-vocab/families.schema.json`); the spec `EXTENDS` the
generated module that holds its table. A `models:` declaration is any Lean
definition the spec restates without a table; naming it selects the spec when
its source changes. A crate is a workspace package; a `paths:` entry is a file
or directory outside every crate, and a change to it or under it selects the
spec. These headers are the
record of expected results; `scripts/tla_check.py check-headers` and
`scripts/ci/test_gate.py` check them: every owner is in the snapshot, every
`models:` declaration and path exists, every crate is a package, every config has a
header, and every `LeanDecision_*` module a spec extends exists and holds one
of its `lean:` owners.

`scripts/tla_check.py run --all` model-checks every config in breadth-first
mode with one worker per config and a fixed fingerprint, compares each result
with its header and prints a table; a mismatch, too few states, a missing
header, a TLC error or a timeout (`--timeout`, seconds per config) fails it.
`--spec`, `--config`, `--crate`, `--lean` and `--files` narrow the run. The
pull request gate runs the specs whose headers name a changed crate or Lean
owner, declaration or path, or whose own files or generated imports changed; main
and the nightly `TLA+ nightly` workflow run every config. TLC's outcome is
recorded under a digest of the script, the pinned tools, the config and every
module it reads, in `~/.cache/gents-tla/results`; a run reuses a recorded
outcome for the same digest and judges it against the header again. The
nightly passes `--fresh`, which runs TLC on every config regardless.
`scripts/tla_check.py install` installs the pinned JDK and `tla2tools.jar`
(version and SHA-256 in the script) under `~/.cache/gents-tla`.
`scripts/run-tlc.sh <MC config>` runs TLC with them and prints its full
output, counterexample included.

## Generated Lean decisions

`make lean-regen` writes `generated/LeanDecision_<Namespace>.tla` for every
`lean:` owner: `<name>_table`, the decision over its whole domain keyed by the
tuple of its arguments in Lean parameter order and written as a balanced tree
of `@@` (a long left-nested chain overflows TLC's stack), and `<name>(...)`,
its lookup. Regeneration fails for an owner that is not such a function. A
spec `EXTENDS` that module instead of restating the decision, and must not
define an operator of the same name. `generated_tla_modules_are_current` in
`gents-lean-vocab` fails when a module is stale. The scripts put `generated/`
on TLC's library path (`-DTLA-Library`).
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

For LeaseRenewalStarvation — the green model (the synchronous step before
the claim) and the `BoundedSync`, `OffRuntime` and `Fenced` models check
clean; the three diagnostics are EXPECTED to report the violation named in
their config:
```bash
./scripts/run-tlc.sh MCLeaseRenewalStarvation
./scripts/run-tlc.sh MCLeaseRenewalStarvationBoundedSync
./scripts/run-tlc.sh MCLeaseRenewalStarvationOffRuntime
./scripts/run-tlc.sh MCLeaseRenewalStarvationFenced
./scripts/run-tlc.sh MCLeaseRenewalStarvationSyncInsideLease
./scripts/run-tlc.sh MCLeaseRenewalStarvationMarginSpent
./scripts/run-tlc.sh MCLeaseRenewalStarvationUnfenced
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

For RuntimeConnectionTransport — the four green models check clean, the
first three including the four liveness properties:
```bash
./scripts/run-tlc.sh MCRuntimeConnectionTransport
./scripts/run-tlc.sh MCRuntimeConnectionTransportEnvelope
./scripts/run-tlc.sh MCRuntimeConnectionTransportHandshake
./scripts/run-tlc.sh MCRuntimeConnectionFlap
```

Its ten diagnostics are EXPECTED to report violations. `OmittedRoot`
violates `NoCauselessReconnect` and `OmittedRootLiveness` violates
`EventuallyConnected` (the revoked-User retry storm), `UncappedBackoff`
violates `ReArmedWithinCap`, `ResetOnConnect` violates `FlapBounded`,
`EarlyRestart` violates `ReconnectsWaitTheBackoff`, `RenewalFlap` violates
`RenewalNeverDisconnects`, `BreakBeforeMake` and `EarlyReady` violate
`ConnectedAfterArmedSnapshot`, `NoEndpoint` violates `NoEndpointNamed` and
`StaleGeneration` violates `StaleNoticeInert`:
```bash
./scripts/run-tlc.sh MCRuntimeConnectionOmittedRoot
./scripts/run-tlc.sh MCRuntimeConnectionOmittedRootLiveness
./scripts/run-tlc.sh MCRuntimeConnectionUncappedBackoff
./scripts/run-tlc.sh MCRuntimeConnectionResetOnConnect
./scripts/run-tlc.sh MCRuntimeConnectionEarlyRestart
./scripts/run-tlc.sh MCRuntimeConnectionBreakBeforeMake
./scripts/run-tlc.sh MCRuntimeConnectionRenewalFlap
./scripts/run-tlc.sh MCRuntimeConnectionNoEndpoint
./scripts/run-tlc.sh MCRuntimeConnectionStaleGeneration
./scripts/run-tlc.sh MCRuntimeConnectionEarlyReady
```

For WindowIndependence — the green model (two windows, two nodes) checks
clean; `NoCancelOnSwitch` violates `NoCrossDelivery`:
```bash
./scripts/run-tlc.sh MCWindowIndependence
./scripts/run-tlc.sh MCWindowIndependenceNoCancelOnSwitch
```

For ObserverResync — the green model (two nodes, a renewal during every
load on both, a write, a drop and a direct refresh per node) checks clean;
`ReconnectGap` and `RejectDelay` are passing diagnostics:
```bash
./scripts/run-tlc.sh MCObserverResync
./scripts/run-tlc.sh MCObserverResyncReconnectGap
./scripts/run-tlc.sh MCObserverResyncRejectDelay
```

Its six diagnostics are EXPECTED to report violations: `SustainedEvents`
and `NodeScopeOnly` violate `EventualPublication` and `Convergence`;
`NoPublicationFence`, `NoEventQueue` and `SnapshotBeforeArm` violate
`NoLostWrite`; `StaleCommit` violates `NoStaleCommit`:
```bash
./scripts/run-tlc.sh MCObserverResyncSustainedEvents
./scripts/run-tlc.sh MCObserverResyncNodeScopeOnly
./scripts/run-tlc.sh MCObserverResyncNoPublicationFence
./scripts/run-tlc.sh MCObserverResyncNoEventQueue
./scripts/run-tlc.sh MCObserverResyncSnapshotBeforeArm
./scripts/run-tlc.sh MCObserverResyncStaleCommit
```

For ObserverCollectionReload — the green model (a write per collection, a
touch and a renewal during every full load, a drop, a direct refresh and a
failed collection read) checks clean:
```bash
./scripts/run-tlc.sh MCObserverCollectionReload
```

Its four diagnostics are EXPECTED to report violations: `IgnoreFence`
violates `NoStaleCommit`; `DropRejected` and `DropFailure` violate
`NoLostWrite`; `ConcurrentReads` violates `EventualPublication`:
```bash
./scripts/run-tlc.sh MCObserverCollectionReloadIgnoreFence
./scripts/run-tlc.sh MCObserverCollectionReloadDropRejected
./scripts/run-tlc.sh MCObserverCollectionReloadDropFailure
./scripts/run-tlc.sh MCObserverCollectionReloadConcurrentReads
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

For StoreHandoff — the green model checks clean; the five diagnostics are
EXPECTED to report the violation named in their config:
```bash
./scripts/run-tlc.sh MCStoreHandoff
./scripts/run-tlc.sh MCStoreHandoffCurrent
./scripts/run-tlc.sh MCStoreHandoffAckBeforeRelease
./scripts/run-tlc.sh MCStoreHandoffRefuseStopping
./scripts/run-tlc.sh MCStoreHandoffUnboundedWait
./scripts/run-tlc.sh MCStoreHandoffPidFile
```

For DroppedPermitCleanup and ClientIdentityMetadata — the green models
(`MCDroppedPermitCleanup`, `Changes`, `ChangesTerminal`;
`MCClientIdentityMetadata`) check clean; every other config is a diagnostic
EXPECTED to report the violation named in its file:
```bash
for m in MCDroppedPermitCleanup*.cfg MCClientIdentityMetadata*.cfg; do
  ./scripts/run-tlc.sh "${m%.cfg}"
done
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
| `MCLeaseRenewalStarvation.cfg` | `Lease = 4`, `Horizon = 14`, `MaxSync = 8`, step before the claim, fenced | Passes `TypeOK`, `SingleWriter`, `OwnerPublishes`, `LeaseHeld` | 2,511 distinct states; 3,651 generated | 33 | <1s (Linux x86_64, OpenJDK 17.0.21, 4 workers) |
| `MCLeaseRenewalStarvationBoundedSync.cfg` | as green, step inside the lease, `MaxSync = 1` | Passes the same; at `Lease = 8`, `Horizon = 20`, `MaxSync = 3` also passes | 2,001 distinct states; 3,010 generated | 33 | <1s |
| `MCLeaseRenewalStarvationOffRuntime.cfg` | as green, step inside the lease, off the runtime | Passes the same | 3,939 distinct states; 7,635 generated | 33 | <1s |
| `MCLeaseRenewalStarvationFenced.cfg` | as green, step inside the lease | Passes `TypeOK`, `SingleWriter` | 12,190 distinct states; 21,739 generated | 33 | <1s |
| `MCLeaseRenewalStarvationSyncInsideLease.cfg` | as `Fenced` | `OwnerPublishes` intentionally VIOLATED: Claim, SyncStart, Tick past the deadline, SyncEnd, Publish rejected | 257 distinct states before counterexample; 306 generated | 11 | <1s |
| `MCLeaseRenewalStarvationMarginSpent.cfg` | as `BoundedSync`, `MaxSync = 2` | `LeaseHeld` intentionally VIOLATED: Claim, Poll skips, SyncStart as the renewal comes due, Tick to the deadline, SyncEnd; at `Lease = 8`, `MaxSync = 4` also fails | 540 distinct states before counterexample; 704 generated | 14 | <1s |
| `MCLeaseRenewalStarvationUnfenced.cfg` | as `Fenced`, `FenceDeadline = FALSE` | `SingleWriter` intentionally VIOLATED: the starved owner's publication after its deadline is accepted | 228 distinct states before counterexample; 275 generated | 11 | <1s |
| `MCClientSubmissionContention.cfg` | `Publications = 3`, `Attempts = 4`, `Redrive`, `ReserveRedrive`, `LostReply`, `Refusals`, `MaxSends = 3`, no `RetryFreshId`, no `PayloadLost` | Passes `TypeOK`, `NoConflictLoss`, `CreatedOnce`, `RetainedUntilConfirmed`, `FailedOffersRetry`, `PendingResolves`, `EverySendSettles` | 33,960 distinct states; 219,232 generated | 23 | 3s (OpenJDK 25, 4 workers) |
| `MCClientSubmissionContentionSingleAttempt.cfg` | `Publications = 1`, `Redrive = FALSE` | `NoConflictLoss` intentionally VIOLATED: Compose, ReserveCommit, AgentPublish, SubmitCommit | 36 distinct states; 58 generated | 8 | <1s |
| `MCClientSubmissionContentionReserveSingleAttempt.cfg` | `Publications = 1`, `ReserveRedrive = FALSE` | `NoConflictLoss` intentionally VIOLATED: Compose, AgentPublish, ReserveCommit | 36 distinct states; 58 generated | 8 | <1s |
| `MCClientSubmissionContentionShortBudget.cfg` | `Publications = Attempts = 4` | `NoConflictLoss` intentionally VIOLATED: four AgentPublish/ReserveCommit rounds | 237 distinct states; 513 generated | 12 | <1s |
| `MCClientSubmissionContentionRetryFreshId.cfg` | `Publications = 0`, `LostReply`, `MaxSends = 2`, `RetryFreshId` | `CreatedOnce` intentionally VIOLATED: Compose, Timeout, ReserveCommit, SubmitCommit (reply not yet received), Retry under a new identity, ReserveCommit, SubmitCommit | 48 distinct states before counterexample; 75 generated | 9 | <1s |
| `MCClientSubmissionContentionNoPayload.cfg` | `Publications = 0`, `LostReply`, `MaxSends = 2`, `PayloadLost` | `FailedOffersRetry` intentionally VIOLATED: Compose, DropPayload, Timeout leaves a failed message with no Retry | 32 distinct states before counterexample; 47 generated | 4 | <1s |
| `MCInterruptLatchContention.cfg` | `Writes = 2`, `OtherInterrupt`, `Attempts = 4`, `Redrive`, `Reread` | Passes `TypeOK`, `NoConflictLoss`, `StampedOnce`, `DoneIsLatched`, `InterruptSettles` | 78 distinct states; 100 generated | — | <1s (OpenJDK 17, 4 workers) |
| `MCInterruptLatchContentionSingleAttempt.cfg` | `Writes = 1`, `Redrive = FALSE` | `NoConflictLoss` intentionally VIOLATED: Open, Renew, Commit | 9 distinct states; 9 generated | 4 | <1s |
| `MCInterruptLatchContentionStaleRebuild.cfg` | `Writes = 0`, `OtherInterrupt`, `Reread = FALSE` | `StampedOnce` intentionally VIOLATED: Open, OtherLatch, Commit (rebuild), Commit stamps again | — | 5 | <1s |
| `MCInterruptLatchContentionExhausted.cfg` | `Writes = Attempts = 4` | `NoConflictLoss` intentionally VIOLATED: four Renew/Commit rounds | — | 9 | <1s |
| `MCSessionHeadObservation.cfg` | `Order = "lean"`, `HeadSeed = TRUE`, `Refusals = TRUE` | Passes `TypeOK`, `HeadFollowsBoundary`, `ReplyObserved` | 838 distinct states; 1,406 generated | 11 | <1s |
| `MCSessionHeadObservationRequestIdTie.cfg` | `Order = "requestId"`, `Refusals = FALSE` | `HeadFollowsBoundary` intentionally VIOLATED: ClaimA, CompleteA, AdmitB, ClaimB leaves the head on A | 77 distinct states before counterexample; 99 generated | 6 | <1s |
| `MCSessionHeadObservationRequestIdTieLiveness.cfg` | `Order = "requestId"`, `Refusals = FALSE` | `ReplyObserved` intentionally VIOLATED: the #3604 schedule; after reopen B completes and every load omits it | 620 distinct states; 1,056 generated | 11 | <1s |
| `MCSessionHeadObservationNoLivePreference.cfg` | `Order = "noLive"`, `Refusals = FALSE` | `ReplyObserved` intentionally VIOLATED: B's claim ranks below A's same-second terminalization | 620 distinct states; 1,056 generated | 11 | <1s |
| `MCSessionHeadObservationNoTerminalizationOrder.cfg` | `Order = "noTerminal"`, `Refusals = TRUE` | `ReplyObserved` intentionally VIOLATED: B refused in A's second loses the random tie | 838 distinct states; 1,406 generated | 11 | <1s |
| `MCSessionHeadObservationNoHeadSeed.cfg` | `Order = "lean"`, `HeadSeed = FALSE` | `ReplyObserved` intentionally VIOLATED: the head is B but the reader reads only nonterminal rows | 572 distinct states; 984 generated | 11 | <1s |
| `MCRuntimeConnectionTransport.cfg` | `K = 2`, `MaxGen = 4`, `Flat = 1`, `Cap = 4`, `MaxAttempts = 4`, `MaxEnv = 3`, `MaxChan = 2`, `Revocation = "served"`, all guards | Passes `TypeOK`, `ConnectedAfterArmedSnapshot`, `RemovalClears`, `NoEndpointNamed`, `RenewalNeverDisconnects`, `NoCauselessReconnect`, `ReArmedWithinCap`, `FlapBounded`, `ReconnectsWaitTheBackoff`, `StaleNoticeInert`, `EventuallyConnected`, `RecoveredEventuallyArmed`, `RefusalNamed`, `NoEndpointEventuallyNamed` | 62,330 distinct states; 442,448 generated | 71 | 7s |
| `MCRuntimeConnectionTransportEnvelope.cfg` | as green, `Revocation = "envelope"` | Passes every invariant and property of the green row | 165,696 distinct states; 1,026,416 generated | 91 | 16s |
| `MCRuntimeConnectionTransportHandshake.cfg` | as green, `Revocation = "handshake"` | Passes every invariant and property of the green row | 91,646 distinct states; 585,810 generated | 71 | 10s |
| `MCRuntimeConnectionFlap.cfg` | as green, `Drops = TRUE` | Passes `TypeOK`, `ConnectedAfterArmedSnapshot`, `RemovalClears`, `NoEndpointNamed`, `NoCauselessReconnect`, `ReArmedWithinCap`, `FlapBounded`, `ReconnectsWaitTheBackoff` | 458,350 distinct states; 3,657,390 generated | 93 | 5s |
| `MCRuntimeConnectionOmittedRoot.cfg` | as green, `OmittedIsEmpty = FALSE` | `NoCauselessReconnect` intentionally VIOLATED: revoked grant, renewal resnapshot, the omitted root read as a failure reconnects a reachable runtime | 3,822 distinct states before counterexample; 16,713 generated | — | <1s |
| `MCRuntimeConnectionOmittedRootLiveness.cfg` | as `OmittedRoot`, liveness only | `EventuallyConnected` intentionally VIOLATED: the revoked User loops through reconnects and never settles on connected | 117,606 distinct states; 755,888 generated | — | 7s |
| `MCRuntimeConnectionUncappedBackoff.cfg` | as green, `Capped = FALSE` | `ReArmedWithinCap` intentionally VIOLATED: three failed reconnects to an unreachable runtime (1, 2, 4 ticks), the runtime recovers while the source waits 8 ticks, and 5 ticks pass with no re-arm | 37,400 distinct states before counterexample; 263,209 generated | — | <1s |
| `MCRuntimeConnectionResetOnConnect.cfg` | as `Flap`, `ResetOnConnect = TRUE` | `FlapBounded` intentionally VIOLATED: arm, snapshot (count reset), drop, reconnect at the base delay, arm, snapshot, drop, a second base-delay reconnect | 22,318 distinct states before counterexample; 133,143 generated | — | <1s |
| `MCRuntimeConnectionEarlyRestart.cfg` | as green, `KeepEnded = FALSE` | `ReconnectsWaitTheBackoff` intentionally VIOLATED: the handshake fails, an unrelated directory change reconciles before the end notice, and the source restarts at once | 1,053 distinct states before counterexample; 3,857 generated | — | <1s |
| `MCRuntimeConnectionBreakBeforeMake.cfg` | as green, `MakeBeforeBreak = FALSE` | `ConnectedAfterArmedSnapshot` intentionally VIOLATED: Connected while the renewal re-arm has no subscription armed | 3,854 distinct states before counterexample; 16,871 generated | — | <1s |
| `MCRuntimeConnectionRenewalFlap.cfg` | as green, `RenewalAsFailure = TRUE` | `RenewalNeverDisconnects` intentionally VIOLATED: arm, ready, renewal, failure notice, Disconnected with the runtime reachable | 2,433 distinct states before counterexample; 9,253 generated | — | <1s |
| `MCRuntimeConnectionNoEndpoint.cfg` | as green, `NamedNoEndpoint = FALSE` | `NoEndpointNamed` intentionally VIOLATED: the peer loses its endpoint and reconcile leaves no status | 46 distinct states before counterexample; 116 generated | — | <1s |
| `MCNativeCompletionHandshake.cfg` | `Timeout = 1`, `MaxAdvance = 1`, `RequireArmed = TRUE`, `ObserveTelemetry = FALSE`, `PermitSemantics = TRUE` | Passes `TypeOK`, `AdvanceFollowsArm`, `ObservationImpliesAbandonment`, `ObservableWithoutRealTime` | 51 distinct states; 104 generated | 10 | <1s (OpenJDK 26) |
| `MCNativeCompletionHandshakeUnarmedAdvance.cfg` | as green, `RequireArmed = FALSE` | `AdvanceFollowsArm` intentionally VIOLATED: DriverPause, DriverAdvance before the caller is polled (`ObservableWithoutRealTime` fails on the same schedule) | 44 distinct states before counterexample; 81 generated | 6 | <1s (OpenJDK 26) |
| `MCNativeCompletionHandshakeTelemetrySignal.cfg` | as green, `ObserveTelemetry = TRUE` | `ObservableWithoutRealTime` intentionally VIOLATED: SuppressRecord, DriverAdvance, CallerExpire leaves the awaited event unemitted | 45 distinct states; 94 generated | 10 | <1s (OpenJDK 26) |
| `MCNativeCompletionHandshakeLostWakeup.cfg` | as green, `PermitSemantics = FALSE` | `ObservableWithoutRealTime` intentionally VIOLATED: CallerExpire lands before DriverWait and never wakes it | 48 distinct states; 99 generated | 10 | <1s (OpenJDK 26) |
| `MCStoreHandoff.cfg` | `Acquirers = {a1, a2}`, `CanCrash`, `CanHang`, every guard on | Passes `TypeOK`, `NeverTwoHolders`, `AckFollowsRelease`, `AckedHandoff`, `RefusalNamesLiveHolder`, `CrashedHolderNeverBlocks`, `AcquirerSettles`, `WaiterTakesReleasedStore`, `StopAcknowledged` | 835 distinct states; 1,327 generated | 12 | <1s (OpenJDK 17, 4 workers) |
| `MCStoreHandoffCurrent.cfg` | `AckAfterRelease = MarkReleasing = WaitOnReleasing = FALSE` | `AckedHandoff` intentionally VIOLATED: BeginStop, Ack, Start(a1), TryLock(a1) finds the acknowledged holder still holding the store | 85 distinct states before counterexample; 128 generated | 6 | <1s |
| `MCStoreHandoffAckBeforeRelease.cfg` | as green, `AckAfterRelease = FALSE` | `AckFollowsRelease` intentionally VIOLATED: BeginStop, Ack | 51 distinct states before counterexample; 69 generated | 6 | <1s |
| `MCStoreHandoffRefuseStopping.cfg` | as green, `MarkReleasing = WaitOnReleasing = FALSE` | `RefusalNamesLiveHolder` intentionally VIOLATED: BeginStop, Start(a1), TryLock(a1), Read(a1) refuses a stopping holder | 79 distinct states before counterexample; 119 generated | 7 | <1s |
| `MCStoreHandoffUnboundedWait.cfg` | as green, `BoundedWait = FALSE` | `AcquirerSettles` intentionally VIOLATED: a2 waits on the stopping holder, the holder crashes, a1 takes the store and a2 waits forever | 657 distinct states; 1,050 generated | 11 | <1s |
| `MCStoreHandoffPidFile.cfg` | as green, `KernelReleasesOnExit = FALSE` | `CrashedHolderNeverBlocks` intentionally VIOLATED: Crash, Start(a1), TryLock(a1) | 65 distinct states before counterexample; 92 generated | 6 | <1s |
| `MCRuntimeConnectionStaleGeneration.cfg` | as green, `GenerationCheck = FALSE` | `StaleNoticeInert` intentionally VIOLATED: the replaced source's queued notice moves the new source's status | 220 distinct states before counterexample; 603 generated | — | <1s |
| `MCRuntimeConnectionEarlyReady.cfg` | as green, `ArmBeforeReady = FALSE` | `ConnectedAfterArmedSnapshot` intentionally VIOLATED: ready before any subscription is armed, snapshot, Connected | 911 distinct states before counterexample; 2,844 generated | — | <1s |
| `MCWindowIndependence.cfg` | `Windows = {w1, w2}`, `Nodes = {n1, n2}`, `MaxWrites = 2`, `MaxSwitch = 3`, per-window fallback, cancel on switch | Passes `TypeOK`, `NoCrossDelivery`, `Converges`, `WatchFollowsSelection` | 126,444 distinct states; 753,497 generated | 20 | 10s |
| `MCWindowIndependenceNoCancelOnSwitch.cfg` | as green, `CancelOnSwitch = FALSE` | `NoCrossDelivery` intentionally VIOLATED: the old node's watch delivers to the switched window | 711 distinct states before counterexample; 1,396 generated | 6 | <1s |
| `MCObserverResync.cfg` | 2 nodes, `MaxWrites = 1`, `K = 1`, `MaxDrops = 1`, `MaxRefreshes = 1`, node-scoped events off the reload fence, immediate requeue | Passes `TypeOK`, `NoLostWrite`, `NoReadWithoutReason`, `NoStaleCommit`, `EventualPublication`, `Convergence` | 2,835,856 distinct states; 16,250,601 generated | 33 | 7min 5s |
| `MCObserverResyncRejectDelay.cfg` | 2 nodes, renewals during every load on both, no drops or refreshes, the 250 ms wait after a rejected load | Passes every invariant and property: the wait is not a guard | 5,329 distinct states; 21,171 generated | 23 | <1s |
| `MCObserverResyncSustainedEvents.cfg` | 1 node, renewals during every load, events on the reload fence, 250 ms retry | `EventualPublication` and `Convergence` intentionally VIOLATED: Arm, Start, Write, Deliver, Read, Publish (rejected), Timer, then Start, Renew, Deliver, Read, Publish (rejected) forever | 118 distinct states; 229 generated | 17 | <1s |
| `MCObserverResyncReconnectGap.cfg` | 2 nodes, `MaxWrites = 2`, `MaxDrops = 2`, no renewals, the code before the fix | Passes every invariant and property: writes between a drop and its re-arm are read by the post-arm snapshot | 243,049 distinct states; 1,010,651 generated | 33 | 14s |
| `MCObserverResyncNodeScopeOnly.cfg` | 1 node, node-scoped events still on the reload fence, immediate requeue | `EventualPublication` and `Convergence` intentionally VIOLATED: the node's own renewals reject each load | 112 distinct states; 220 generated | 17 | <1s |
| `MCObserverResyncNoPublicationFence.cfg` | 1 node, publications do not fence reloads; `NoStaleCommit` unchecked | `NoLostWrite` intentionally VIOLATED: the observer publishes its newer read, then the older direct refresh publishes over it | 144 distinct states before counterexample; 260 generated | 12 | <1s |
| `MCObserverResyncStaleCommit.cfg` | `NoPublicationFence` checking only `TypeOK` and `NoStaleCommit` | `NoStaleCommit` intentionally VIOLATED: the same older refresh replaces the newer published version | 114 distinct states before counterexample; 201 generated | 9 | <1s |
| `MCObserverResyncNoEventQueue.cfg` | 1 node, a delivered event does not queue its node | `NoLostWrite` intentionally VIOLATED: a write after the snapshot read is delivered and never loaded | 21 distinct states before counterexample; 25 generated | 7 | <1s |
| `MCObserverResyncSnapshotBeforeArm.cfg` | 1 node, the snapshot is queued before the streams arm | `NoLostWrite` intentionally VIOLATED: a write between the read and the arm has no event and no later load | 65 distinct states before counterexample; 102 generated | 7 | <1s |
| `MCSharedOwnerHotCounter.cfg` | `Writers = {w1, w2, w3}`, `Iters = 2`, `Attempts = 4`, `Capacity = 2`, commit timeouts | Passes `TypeOK`, `AckedApplied`, `ConflictNotApplied`, `NoDoubleApply`, `NoLostUpdate`, `TestAccounting`, `AttemptBound`, `PermitsAccounted`, `EveryCallSettles` | 86,973 distinct states; 171,709 generated | — | 30s (Linux, OpenJDK 17, 8 workers) |
| `MCSharedOwnerHotCounterUnbounded.cfg` | as green, `BoundedRetry = FALSE`, no timeouts | Passes every invariant but `AttemptBound` (unchecked) and `EveryCallSettles` | 8,428 distinct states; 18,517 generated | — | 2s |
| `MCSharedOwnerHotCounterReadOutsideTxn.cfg` | as green, read before begin | `NoLostUpdate` intentionally VIOLATED | 144 distinct states; 186 generated | — | <1s |
| `MCSharedOwnerHotCounterStaleRetry.cfg` | as green, the retry keeps the aborted read | `NoLostUpdate` intentionally VIOLATED | 297 distinct states; 465 generated | — | <1s |
| `MCSharedOwnerHotCounterConflictAsSuccess.cfg` | as green, an abort acknowledged | `AckedApplied` intentionally VIOLATED | 126 distinct states; 181 generated | — | <1s |
| `MCSharedOwnerHotCounterRetryAmbiguous.cfg` | as green, a timed-out commit repeated | `NoDoubleApply` intentionally VIOLATED | 1,354 distinct states; 2,427 generated | — | <1s |
| `MCSharedOwnerHotCounterLostWakeup.cfg` | as green, a parked acquirer never woken | `EveryCallSettles` intentionally VIOLATED | 15,628 distinct states; 30,262 generated | — | 1s |
| `MCSharedOwnerHotCounterAbortOnOpenWriter.cfg` | as green, aborts for uncommitted writers, unbounded retries | `EveryCallSettles` intentionally VIOLATED: a livelock lasso | 22,124 distinct states; 56,227 generated | — | 2s |
| `MCSharedOwnerHotCounterWaveDeadline.cfg` | as green, `CheckDeadline`, `Deadline = 2` | `WaveDeadline` intentionally VIOLATED: three ticks pass before any writer runs; every safety invariant holds | 226 distinct states; 386 generated | 4 | <1s |
| `MCSharedOwnerHotCounterNative.cfg` | `Iters = 1`, `Attempts = 1`, `Capacity = 3`, no timeouts | Passes every invariant, `RoundProgress` and `EveryCallSettles` | 195 distinct states; 304 generated | — | <1s |
| `MCSharedOwnerHotCounterNativeAbortOnOpenWriter.cfg` | as `Native`, two writers, overlapping writers aborted together | `RoundProgress` intentionally VIOLATED: both writers read, both abort | 26 distinct states; 32 generated | — | <1s |
| `MCBackgroundLiveOutputObservation.cfg` | no child self-expiry, no poll budget | Passes `TypeOK`, `ReadAgrees`, `NoWindowFailure`, `CompletedOk`, `Completes` | 7 distinct states; 8 generated | — | <1s (Linux, OpenJDK 17) |
| `MCBackgroundLiveOutputObservationTimed.cfg` | `SelfExpire`, `K = 2` | `ReadAgrees` intentionally VIOLATED: ChildPrint, Tick, Tick, Poll (running, 4 bytes), ChildFinish, Read returns 8 | 20 distinct states; 25 generated | 7 | <1s |
| `MCBackgroundLiveOutputObservationTimeoutKill.cfg` | `SelfExpire`, `K = 2` | `CompletedOk` intentionally VIOLATED: the tool timeout kills the child at its gate after the running/4 listing; the wait reports failed | 31 distinct states; 39 generated | 9 | <1s |
| `MCBackgroundLiveOutputObservationPollBudget.cfg` | `PollCap`, `Cap = 2` | `NoWindowFailure` intentionally VIOLATED: two polls before the child prints | 6 distinct states; 6 generated | 3 | <1s |
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
| `MCDroppedPermitCleanup.cfg` | `Calls = {c1, c2}`, every guard on, graceful, deadline and abrupt exits, reader on the flush | Passes `TypeOK`, `ExactlyOneTerminal`, `ObserverSawTarget`, `GracefulExitFlushed`, `EndedCallTerminal` | 457 distinct states; 1,048 generated | 11 | 1s (Linux x86_64, OpenJDK 17, 4 workers) |
| `MCDroppedPermitCleanupChanges.cfg` | as green without exits, reader on the change subscription for persisted rows | Passes `TypeOK`, `ExactlyOneTerminal`, `ObserverSawTarget`, `ObserverSettles` | 467 distinct states; 1,661 generated | 11 | 1s |
| `MCDroppedPermitCleanupChangesTerminal.cfg` | as `Changes`, for terminal rows | Passes the same | 419 distinct states; 1,509 generated | 9 | 1s |
| `MCDroppedPermitCleanupUntrackedSpawn.cfg` | `Tracked = FALSE` | `ObserverSawTarget` intentionally VIOLATED: Enqueue, Drop for both calls, ObsWait, ObsFlushed reads live rows | 102 distinct states before counterexample; 250 generated | 7 | <1s |
| `MCDroppedPermitCleanupUnawaitedFlush.cfg` | `ShutdownAwaitsFlush = FALSE` | `GracefulExitFlushed` intentionally VIOLATED: Enqueue(c1), BeginShutdown, Drop(c1), Exit | 80 distinct states before counterexample; 132 generated | 6 | <1s |
| `MCDroppedPermitCleanupNoRecovery.cfg` | `Recovery = FALSE` | `EndedCallTerminal` intentionally VIOLATED: a queued call's runtime exits abruptly and its row stays live | 457 distinct states; 808 generated | 11 | <1s |
| `MCDroppedPermitCleanupNoTerminalCas.cfg` | `TerminalCas = FALSE` | `ExactlyOneTerminal` intentionally VIOLATED: Start, FinishCommit, Drop, RunCleanup | 165 distinct states before counterexample; 281 generated | 6 | <1s |
| `MCDroppedPermitCleanupSleepTerminal.cfg`, `SleepQueued.cfg` | `Signal = "timer"` | `ObserverSawTarget` intentionally VIOLATED: the timer fires first | 2 distinct states | 2 | <1s |
| `MCDroppedPermitCleanupQueuedByFlush.cfg` | flush awaited for persisted rows | `ObserverSawTarget` intentionally VIOLATED: ObsWait, ObsFlushed before any row exists | 46 distinct states before counterexample; 65 generated | 4 | <1s |
| `MCDroppedPermitCleanupPollBudget.cfg` | `Signal = "poll"`, `PollBudget = 2` | `ObserverNeverTimesOut` intentionally VIOLATED: two polls before any write | 21 distinct states before counterexample; 24 generated | 4 | <1s |
| `MCDroppedPermitCleanupLateSubscribe.cfg` | `SubscribeFirst = FALSE` | `ObserverSettles` intentionally VIOLATED: ObsRead, the last write, ObsSubscribe, then no wake | 671 distinct states; 2,303 generated | 11 | 1s |
| `MCClientIdentityMetadata.cfg` | `Writers = {w1, w2, w3}`, every guard on, `CanCrash` | Passes `TypeOK`, `NoMissingFileError`, `NoTornMetadata`, `NeverUninstalled`, `OkSawOwnMetadata`, `AgreedInstall`, `WriterSettles`, `SameKeyAccepts` | 856 distinct states; 2,743 generated | 14 | 1s |
| `MCClientIdentityMetadataBefore.cfg` | shared staging name, removal, no second read | `NoMissingFileError` intentionally VIOLATED: w1 stages and removes, w2 truncates the same staging file, w1 renames it away, w2's rename finds it gone | 70 distinct states before counterexample; 95 generated | 11 | <1s |
| `MCClientIdentityMetadataSharedStaging.cfg`, `SharedStagingTorn.cfg` | as green, `UniqueStaging = FALSE` | `NoMissingFileError` and `NoTornMetadata` intentionally VIOLATED | 49 and 52 distinct states | 10 | <1s |
| `MCClientIdentityMetadataRemoveThenRename.cfg`, `RemoveThenRenameReread.cfg` | as green, `RemoveBeforeRename = TRUE` | `NeverUninstalled` and `NoMissingFileError` intentionally VIOLATED: w2 removes the file w1 installed | 52 and 55 distinct states | 12 | <1s |
| `MCClientIdentityMetadataNoReread.cfg` | `SingleWinnerKey = FALSE`, `Reread = FALSE` | `OkSawOwnMetadata` intentionally VIOLATED | 46 distinct states | 11 | <1s |
| `MCClientIdentityMetadataDivergentKey.cfg` | `SingleWinnerKey = FALSE` | `AgreedInstall` intentionally VIOLATED: w1 accepts, w2 renames over it | 42 distinct states | 11 | <1s |

The nine BackgroundAdmissionContention rows used TLC2 `2026.11.06.222357`
(rev `d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and
`-workers 2`. The green run checked its temporal property over the complete
state space. The counts before each counterexample vary with the worker
count.

The six SessionHeadObservation rows used TLC2 `2026.11.06.222357` (rev
`d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and `-workers 2`; the
liveness rows checked `ReplyObserved` over the complete state space.

The fourteen RuntimeConnectionTransport rows used TLC2 `2026.11.06.222357` (rev
`d8fb231`) with Homebrew OpenJDK 25.0.2 on macOS arm64 and `-workers 2`; the
green rows checked their temporal properties over the complete state space.

The nine ObserverResync rows used TLC2 `2026.11.06.222357` (rev `d8fb231`)
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
