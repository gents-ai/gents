# Issue: Harden Iroh Reconnection, Peer Health, and Replication Diagnostics

## Summary
`defra-agent-desktop` already bootstraps saved peers, retries `connect_peer`, installs replicators, and periodically repairs broken pairings. That is enough for happy-path demos, but it is not enough for long-lived embedded or desktop deployments where network conditions change, peers restart, or replication degrades without a clean disconnect.

Recent live logs from a runtime peer and a desktop peer show a partial-failure mode:

- peers do connect and data does eventually move
- the Iroh gossip fast path emits repeated decode failures
- transport/address lookup emits relay and routing warnings
- replication hits retriable storage conflicts
- the desktop/UI only has a binary notion of `saved only` vs `dialed / replication armed`

We should make the desktop peer maintenance loop more explicit about what it is repairing, surface richer health state in the UI, and add hooks for network-change-driven recovery.

## Evidence

### Current desktop code paths
- Bootstrap and one-off add-peer flow call `connect_peer` and `add_replicator` directly in:
  - `crates/defra-agent-desktop/src/client/core.rs`
  - `ClientCore::add_peer`
- Bootstrap retry logic already exists in:
  - `connect_peer_with_retry`
  - `add_replicator_with_retry`
  - `wait_for_connected_peer`
- Periodic repair already exists in:
  - `spawn_peer_maintenance_task`
  - `needs_pairing_repair`
  - `repair_saved_peer`
- Local runtime pairing already retries:
  - `crates/defra-agent-desktop/src/local_runtime.rs`
  - `complete_runtime_pairing`

### Current limitations in the desktop app
- `ClientPeerStatus` only tracks:
  - `dial_succeeded`
  - `last_error`
- The maintenance loop runs every 5 seconds, but it only reasons about:
  - whether the peer appears in `connected_peers`
  - whether the last repair recorded an error
- Peer removal explicitly notes that there is no disconnect operation on the public P2P surface, so active sessions remain until restart.
- Peer and logs views currently flatten health into coarse labels such as:
  - `saved only`
  - `dialed / replication armed`
  - `configured/dialed`

### Runtime / transport symptoms observed in live logs
- Gossip decode failures:
  - `Failed to decode gossip message (version skew or malformed sender?)`
- Storage contention during replication:
  - `blockstore error: storage error: transaction conflict. Please retry`
- Network churn / address instability:
  - `Can't assign requested address`
  - `Address Lookup failed`
  - `No route to host`

These are exactly the kinds of failures where a desktop maintainer should be able to:

- explain which phase is failing
- retry the right operation
- recover automatically after transient network changes

## Problem Statement
Today the desktop can tell whether it once dialed a peer, but not whether that peer is still healthy for actual replicated chat traffic.

We need a stronger model than "saved" vs "connected":

- transport connected
- replicator configured
- local runtime paired
- recently healthy
- degraded but repairable
- permanently misconfigured

Without that, users get silent degradation and operators cannot tell whether a failure is:

- bad address / stale address
- transport disconnect
- local runtime pairing drift
- replicator not installed
- replication stalled after connect
- upstream transport/runtime issue

## Scope
Primary scope is `crates/defra-agent-desktop`.

The work should cover:

- peer bootstrap
- saved-peer repair
- local runtime pairing
- peer diagnostics in the desktop UI
- tests for restart and transient failure recovery

## Proposed Work

### 1. Expand `ClientPeerStatus` into a real peer health model
Update `ClientPeerStatus` to track distinct phases instead of a single `dial_succeeded` bit.

Suggested additions:

- `transport_connected: bool`
- `replicator_installed: bool`
- `runtime_paired: bool`
- `health: enum`
- `last_connected_at: Option<DateTime<Utc>>`
- `last_repair_attempt_at: Option<DateTime<Utc>>`
- `last_healthy_at: Option<DateTime<Utc>>`
- `consecutive_failures: u32`
- `last_error_phase: Option<String>`
- `last_success_phase: Option<String>`

Suggested health states:

- `Saved`
- `Connecting`
- `Connected`
- `ReplicationDegraded`
- `RuntimePairingDegraded`
- `Unhealthy`

### 2. Make peer maintenance phase-aware
Refactor `repair_saved_peer` so it repairs in explicit phases and records which phase failed:

1. verify current transport connectivity
2. redial if disconnected
3. reinstall replicator if required
4. re-run local runtime pairing for local-standard peers
5. clear warnings only after the full required chain succeeds

This should log structured maintenance events under a dedicated tracing target such as:

- `defra_agent_desktop::peer_maintenance`

### 3. Surface richer diagnostics in the UI
Update the peers and logs views so the operator can see:

- current health state
- last failing phase
- last successful repair time
- last observed warning
- whether the peer is configured, connected, and replication-ready

Add explicit operator actions:

- `Repair now`
- `Reconnect`
- `Reinstall replicator`
- `Re-run local runtime pairing`
- `Copy shareable address`

### 4. Prefer stable/shareable addresses for saved peers
Do not assume "the first listen address" is the best recovery address.

At minimum:

- preserve the address/ticket the user entered when saving a peer
- continue preferring loopback-only addresses for local runtime pairing
- avoid replacing a good ticket with a less stable direct-only hint during maintenance

If the runtime exposes multiple addresses, keep enough information to distinguish:

- loopback-local pairing address
- user-facing shareable address
- parsed peer id

### 5. Add a maintenance trigger for network changes
The maintenance loop is currently time-based only.

Add a way for the desktop to trigger immediate repair when the host network changes:

- Wi-Fi changes
- VPN connects/disconnects
- machine wakes from sleep

This is blocked on upstream support in `defradb.rs`, but the desktop issue should wire the client-side trigger and use it when the upstream API exists.

### 6. Tighten the notion of "replication armed"
The current UX tells users to wait for `replication: subscriptions armed`, but that is startup/bootstrap oriented, not a continuing health signal.

We should define and surface a stronger runtime condition for "healthy enough to chat":

- peer connected
- subscribed collections configured
- replicator configured
- no current peer warning

The status bar and peer cards should reflect that condition rather than reusing bootstrap language indefinitely.

## Tests
Add or extend tests in `crates/defra-agent-desktop/src/app/tests` and `crates/defra-agent-desktop/tests` for:

- saved peer reconnects after transient connect failure
- saved peer reconnects after remote restart
- local runtime pairing is re-applied after loss of connectivity
- degraded peer state appears in the UI with the correct failing phase
- successful repair clears warnings and returns the peer to healthy state
- manual `Repair now` action runs the same repair path as background maintenance

If feasible, add a live smoke scenario that:

1. starts desktop + runtime
2. establishes pairing
3. restarts one side
4. verifies the maintenance loop heals the peer without manual peer re-entry

## Acceptance Criteria

- A saved peer that restarts is reconnected automatically without re-entering its address.
- The desktop UI distinguishes at least:
  - `saved`
  - `connecting`
  - `connected`
  - `replication degraded`
  - `unhealthy`
- Peer warnings identify the failing phase, not just a raw transport error string.
- Operator-facing logs include maintenance attempts and outcomes.
- Local runtime pairing can be re-applied automatically after transient failure.
- Tests cover restart recovery and degraded-state surfacing.

## Upstream Support Needed From `defradb.rs`
This desktop issue can improve behavior now, but a few upstream changes will make it materially better:

1. Expose `notify_network_change` on the public P2P surface.
   - The lower-level Iroh transport already has a `network_change()` hook.
   - The public HTTP / embedded / FFI surface does not currently expose it end-to-end.

2. Return a stable/shareable Iroh address more explicitly.
   - Ticket-first or a dedicated `best_shareable_address` would be more reliable than relying on the first listen address.

3. Investigate and fix Iroh gossip decode failures.
   - Live logs showed repeated `Failed to decode gossip message (version skew or malformed sender?)`.
   - The desktop can recover around this, but the transport should not live in a permanently degraded gossip state.

4. Retry or reduce retriable storage conflicts on the Iroh ingress path.
   - Live logs showed repeated `transaction conflict. Please retry` errors during replication handling.

5. Optional: expose richer peer / replicator health diagnostics.
   - Even a minimal "replicator present" or "peer status" API would let the desktop distinguish connect success from replication readiness.

## Non-Goals
- Full peer discovery beyond saved peers and explicit tickets
- A generic disconnect operation, unless the upstream API adds one
- Changing the agent protocol itself

## Suggested Implementation Order

1. Expand `ClientPeerStatus`
2. Refactor maintenance into explicit repair phases
3. Surface richer peer health in UI and logs
4. Add manual repair actions
5. Add restart / recovery tests
6. Wire network-change-triggered repair once upstream support exists
