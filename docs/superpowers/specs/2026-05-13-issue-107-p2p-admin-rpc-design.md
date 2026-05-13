# Issue #107 — P2P-only DefraDB Subscription Management: defra-agent Design

**Status:** Design
**Date:** 2026-05-13
**Tracks:** issue #107 (implementation track); sibling to #155 (cross-boundary verification strategy), #162 (reverse-pairing TLA+), #176 (cross-deployment subagent completion TLA+), #178 (Lean idempotent install/teardown); names follow-ups #166, #168, #170, #180.
**Scope:** defra-agent's half of #107 — the consumer-side reconcile, the transport-agnostic admin client, the operator-visible retry surface, and the migration path from HTTP (today) to a future defradb.rs admin channel. Wire-protocol design for the defradb.rs admin channel itself is the upstream's design problem and is not specified here.

## 1. Goal

Specify defra-agent's #107 work: a consumer-side reconcile loop that honors the leads-to convergence proven in `ReversePairing.tla`, a transport-agnostic `RemoteP2pAdmin` trait that wraps the existing HTTP P2P surface today and a future P2P admin channel later, a per-peer-per-collection retry surface for operator visibility, and a migration path that lets defra-agent keep shipping while defradb.rs's admin transport lands separately. The TLA+ models in `crates/defra-agent/proofs/tla/` formalize the behavior the implementation must preserve; the Lean module `Proofs/ReversePairingHandlers.lean` formalizes the idempotence the implementation must honor.

This spec produces an implementation plan, not implementation. The plan is the next step (writing-plans skill) and must include both Rust and a new Lean module `Proofs/PairingReconcile/*`.

## 2. Why now

The cross-boundary verification thread (#155 → #162 → #176 → #178) has derived obligations on the **consumer** of the P2P pairing substrate. Today's defra-agent reverse-pairing flow in `crates/defra-agent-desktop-core/src/local_runtime/pairing.rs` is one-shot: it dials a remote peer over HTTP, posts to `/p2p/connect`, `/p2p/collections`, `/p2p/replicators` once with a 20 s deadline and a 250 ms backoff, then gives up. There is no reconcile, no retry beyond the deadline, no idempotence story, no operator surface for "this pairing has been stuck for an hour." The TLA+ model proves `(p, c) ∈ desired[n][p] ⤳ (n, c) ∈ replicator[p][n]` under fairness on `Reconcile`; the implementation does not satisfy that fairness assumption.

#107 closes that gap on the consumer side. It is the implementation track for the verification work.

## 3. Verified obligations

Each derived requirement either lands in this spec, lands in a named follow-up, or is a wire-contract obligation on the eventual transport.

| ID | Source | Obligation | Where it lands |
|---|---|---|---|
| #162-R1 | `2026-05-08-reverse-pairing-tla-design.md` §"Derived requirements" | Persist-before-ack on receiver: every state-mutating admin RPC must commit persisted state strictly before emitting the ack. | defradb (Go) HTTP handler + defradb.rs HTTP handler (today's transport); eventual P2P admin channel (later transport). Tracked under #168. Not this spec — wire-contract obligation only. |
| #162-R2 | `2026-05-08-reverse-pairing-tla-design.md` §"Derived requirements" | Idempotent install/teardown handlers: each handler produces the same persisted state regardless of how many times the same logical RPC is delivered. | Discharged in `crates/defra-agent/proofs/Proofs/ReversePairingHandlers.lean` (#178). Consumer-side idempotence follows for free from the reconcile loop's desired-vs-actual diff. |
| #162-R3 | `2026-05-08-reverse-pairing-tla-design.md` §"Derived requirements" | Rust gossipsub subscription persistence parity with Go. | defradb.rs follow-up (#166). Not this spec — upstream obligation. The reverse-pairing user experience depends on it; until #166 lands the durable retry surface (this spec's §7 v2) cannot ship. |
| #162-R4 | `2026-05-08-reverse-pairing-tla-design.md` §"Derived requirements" | Stuck-retry visibility: runtime-visible indicator with elapsed-time, attempt count, last-error class. | **This spec, §7.** **v1: in-memory on `ClientPeerStatus`.** Durable surface deferred to a v2 follow-up gated on #166. #170 acceptance criteria re-scoped accordingly: v1 is in-memory; durable resilience across restarts is v2. |
| #176-R1 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Persist-before-projection on A: projection worker consumes durable local rows, not volatile subscription events. | Future R5 (cross-deployment subagent worker). This spec names the handoff: the substrate produced by #107's consumer reconcile is what R5 reads; #107 does not implement R5. |
| #176-R2 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Persist-before-observe on B: child terminal not observable until durably persisted. | defradb (Go) / defradb.rs upstream + agent runtime. Sibling of #168. |
| #176-R3 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Final response durable before terminal observable. | defradb upstream + agent runtime. |
| #176-R4 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Atomic coalesced wake-up insert. | Future R5. |
| #176-R5 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Projection-side idempotency. | Future R5. |
| #176-R6 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Notification-before-wake-up ordering. | Future R5. |
| #176-R7 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Cancellation drain filters on `source = subagent_completion`. | Future R5. |
| #176-R8 | `2026-05-12-subagent-completion-cross-deployment-tla-design.md` §"Derived requirements" | Late child terminal after cancellation is a no-op. | Future R5. |

Authorization (NAC-bound actor identity) is a wire-contract obligation across all the above. Today's HTTP path doesn't honor it; #180 tracks closing that gap.

## 4. Architecture

### 4.1 HTTP is realization #1 of the TLA+ model

`ReversePairing.tla` is transport-agnostic. The actions (`OperatorWrite`, `Reconcile`, `Send`, `Deliver`, `Drop`, `Process`, `ReceiveAck`, `Timeout`, `Crash`) describe abstract control-plane behavior. They are realized today by HTTP and will be realized later by the defradb.rs admin channel; the proofs apply to whichever transport is live, provided the action mapping holds.

Action mapping for today's HTTP transport (derives the conformance harness IR — see §7.3):

| TLA+ action | HTTP realization in defra-agent + DefraDB |
|---|---|
| `OperatorWrite(n, p, S)` | Operator writes a `PeerPairingDesired` doc (see §6.2) into defra-agent's DefraDB store. |
| `Reconcile(n)` | One tick of `run_saved_peer_repair_cycle` in `crates/defra-agent-desktop-core/src/client/core/supervisor.rs` after extending it with desired-vs-actual diff. |
| `Send(install_rpc)` | `RemoteP2pAdmin::add_p2p_collections` / `::add_replicator` issues an HTTP POST to the remote's `/p2p/collections` or `/p2p/replicators`. |
| `Send(teardown_rpc)` | `RemoteP2pAdmin::delete_p2p_collections` / `::delete_replicator` issues an HTTP DELETE. |
| `Deliver(rpc)` | The remote's HTTP router receives the request and forwards to `handler_p2p.go` / its Rust equivalent. |
| `Drop(rpc)` | TCP / `reqwest` failure before the receiver processes the request. |
| `Process(recv, rpc)` | The receiver's `db.AddP2PCollections` / `db.AddReplicator` / `db.DeleteXxx` runs end-to-end including NAC check, txn commit, and pubsub-topic side-effects. |
| `Ack(rpc)` | HTTP 200 with empty body. |
| `Timeout` | `reqwest` timeout fires on the caller before the response arrives. |
| `Crash(n)` | Process kill / panic / OS reboot. In-memory `ClientPeerStatus` retry state is cleared; `PeerPairingDesired` survives. |

The same mapping applies, line-for-line, when the transport changes to defradb.rs's admin channel. `Send` becomes "construct CBOR request, write to the admin CommChannel stream"; `Ack` becomes "receive the ack message on the response endpoint"; etc. The behavior the proofs require is preserved across the transport change.

### 4.2 New defra-agent components

**(1) `RemoteP2pAdmin` trait.** Sibling to the existing `defra_p2p_adapter::P2POperations` trait. `P2POperations` stays *local-only* (the embedded node's own state — local peer id, local listen addresses, local replicators). `RemoteP2pAdmin` is for talking to a remote peer's admin surface. One method per HTTP operation today (15 ops total, see §8), each taking a peer-identifier parameter (multiaddr or saved peer record reference).

**(2) `HttpRemoteP2pAdmin` impl.** Wraps the calls that `pairing.rs` performs today, generalized to all 15 operations and parameterized by peer-graphql-URL. Replaces `pairing.rs`. Same retry semantics on the per-call level (per-op `RPC_TIMEOUT`, no internal backoff — the supervisor loop is the retry).

**(3) `P2pRemoteP2pAdmin` impl.** Lands when defradb.rs ships its admin CommChannel. Wraps the eventual `defradb_rs::admin::Client` (or equivalent). Same trait surface as `HttpRemoteP2pAdmin`, so the supervisor doesn't care which transport is live.

**(4) Extended supervisor reconcile loop.** `spawn_p2p_supervisor_task` already exists and already runs at `P2P_SUPERVISOR_INTERVAL`. The extension: on each tick, for each saved peer:

1. Read the per-peer desired state from `PeerPairingDesired` (see §6.2).
2. Read the actual remote state by calling `RemoteP2pAdmin::list_p2p_collections` + `::list_replicators`.
3. Compute the diff (set membership) and emit RPCs only for divergence.
4. On RPC failure, increment per-peer-per-collection retry counter; on success, reset.
5. Push updated `ClientPeerStatus` into the `peer_statuses` lock for downstream consumers (desktop UI).

The loop is single-threaded per supervisor task. Multiple peers are reconciled sequentially within a tick. This matches the TLA+ model where `Reconcile(n)` is a single action per firing.

**(5) Extended `ClientPeerStatus`.** Existing struct already tracks `peer_id`, `label`, `agent_did`, `addr`, `dial_succeeded`, `last_error`. v1 of this spec adds per-collection sub-status: `pairing_retry_count`, `last_retry_at`, `last_retry_error_class`, `stuck_since`. In-memory only at v1 (see §7).

**(6) New Lean module `Proofs/PairingReconcile/*`.** Mandatory implementation deliverable. Models the supervisor's reconcile state machine after the pattern in `Proofs/RuntimeReconcile/` and `Proofs/ApplyReconcile/`. Proves the supervisor honors the TLA+ `Reconcile(n)` action semantics. Likely files: `State.lean`, `Transition.lean`, `Executable.lean`, `Convergence.lean`.

### 4.3 Component diagram (text)

```
Operator                                                Remote Peer
   |                                                         ^
   | writes PeerPairingDesired doc                           |
   v                                                         |
+--------------------------+                                 |
| defra-agent DefraDB store|                                 |
|  - PeerPairingDesired    |                                 |
|  - PeerDirectory         |                                 |
|  - ...                   |                                 |
+--------------------------+                                 |
   |                                                         |
   | (DefraDB subscription or tick read)                     |
   v                                                         |
+--------------------------+      ticks at P2P_SUPERVISOR_INTERVAL
| spawn_p2p_supervisor_task|                                 |
|  - desired vs actual     |                                 |
|  - emit divergence RPCs  |--+                              |
|  - retry counters        |  |                              |
+--------------------------+  | dyn RemoteP2pAdmin           |
                              v                              |
                  +---------------------------+              |
                  | HttpRemoteP2pAdmin (now)  |---HTTP------>|
                  | P2pRemoteP2pAdmin (later) |---P2P admin->|
                  +---------------------------+              |
                                                             v
                                                  +-------------------+
                                                  | DefraDB local p2p |
                                                  |  - subscriptions  |
                                                  |  - replicators    |
                                                  +-------------------+
```

## 5. Authorization

The wire-contract obligation, applied to either transport: every admin operation must be NAC-gated against an actor identity. The actor is the operator DID — concretely, the desktop's `PrincipalIdentity` (`crates/defra-agent-desktop-core/src/client/core/bootstrap.rs:52`) — not the agent DID. Operators administer; agents do work; the identity model in CLAUDE.md draws the same boundary.

Today's `pairing.rs` HTTP client builds a bare `reqwest::Client` with no identity header. The NAC option exists in DefraDB but is not wired in defra-agent's deployment. The proofs assume NAC-bound auth as a structural invariant (#162 explicitly defers correctness of "remote only installs subscriptions for authorized actors" to Lean-against-the-handler rather than TLA+); the implementation does not honor that invariant.

This spec **names the gap and defers the fix.** Tracking issue: **#180** ("P2P admin auth: wire actor-DID + NAC into the HTTP path"). The auth deferral has a target; it is not a dangling pointer.

In design terms, the `RemoteP2pAdmin` trait is shaped to make auth wiring drop-in: both impls take an `&PrincipalIdentity` parameter (or hold one via construction), and signing the requests is an impl-level concern that does not change the trait surface. When #180 closes, no caller code changes.

## 6. Persistence and idempotence

### 6.1 Receiver-side persist-before-ack

Wire-contract obligation. The remote's HTTP handler (today) or admin RPC handler (later) must commit the state change to persisted storage strictly before emitting the ack. This is #162-R1. The Go HTTP handler today calls `db.AddP2PCollections` which uses `ensureContextTxn` and commits before returning — that satisfies the obligation on Go's HTTP path. The Rust HTTP equivalent in defradb.rs requires audit; #168 tracks the work to make both implementations explicitly satisfy the obligation in CI tests.

This spec does not deliver the upstream work. It records the obligation as a wire-contract requirement.

### 6.2 Operator desired state — `PeerPairingDesired` collection

A new DefraDB collection in defra-agent's protocol layer. Mirrors the CLAUDE.md principle that the control plane is document-driven.

Proposed schema (final shape defined in the implementation plan):

```graphql
type PeerPairingDesired {
  peer_id: String!                    # libp2p / iroh peer ID
  collections: [String!]!             # collection IDs the operator wants this peer to push to us
  replicator_addresses: [String!]!    # multiaddrs we ask this peer to install replicators against (for outgoing)
  created_at: DateTime!
  updated_at: DateTime!
}
```

Existing pairing UI and CLI surfaces (`crates/defra-agent-desktop-core/src/client/core/writes.rs:213` and the CLI `p2p` commands) update to write this doc as part of their existing flows. The supervisor reads this doc each tick to compute the desired set.

This makes the TLA+ `OperatorWrite(n, p, S)` action a single-doc write at the implementation level — a clean grounding for the conformance harness in §7.3.

### 6.3 Consumer-side idempotence

Consumer-side idempotence follows for free from the reconcile loop. The supervisor's per-tick contract is: read desired, read actual, emit RPCs only for divergence. A retry of a previously-acked install request finds the remote already in the desired state on the next tick's `list_*` read — the diff is empty, no RPC is emitted, no double-install occurs. This matches the set-semantics that `ReversePairingHandlers.lean` (#178) proves on the receiver side: `applyInstall (applyInstall s c) c = applyInstall s c`. The proof is on the receiver; the consumer side honors the same invariant by construction.

The supervisor never persists "I sent this RPC" state beyond the in-memory retry counter. A crash mid-RPC leaves no caller-side trace, and the next tick re-reads desired + actual and reissues only if still needed. This matches `Crash(n)` in the TLA+ model, which clears `inFlight` while preserving `desired` and `replicator`.

## 7. Operator surfaces

### 7.1 v1 — in-memory on `ClientPeerStatus`

The struct extends with per-collection retry sub-status:

```rust
pub struct ClientPeerStatus {
    // existing fields:
    pub peer_id: String,
    pub label: String,
    pub agent_did: Option<String>,
    pub addr: String,
    pub dial_succeeded: bool,
    pub last_error: Option<String>,
    // new fields (v1, in-memory):
    pub pairing: Vec<PairingCollectionStatus>,
}

pub struct PairingCollectionStatus {
    pub collection_id: String,
    pub pairing_retry_count: u32,
    pub last_retry_at: Option<SystemTime>,
    pub last_retry_error_class: Option<PairingErrorClass>,
    pub stuck_since: Option<SystemTime>,
}
```

State is rebuilt deterministically each tick from the desired-vs-actual diff plus the previous tick's counters. Lost on restart — deliberately, because the diff drives retry, not the counter. The counter is for *visibility*, not for *correctness*.

### 7.2 v1 — named parameters

Defaults proposed; values revisited in the implementation plan.

| Parameter | Proposed default | Meaning |
|---|---:|---|
| `RPC_TIMEOUT` | 10 s | Per-op deadline. Matches the comm-channel pattern's synchronous send timeout. |
| `STUCK_THRESHOLD_ATTEMPTS` | 6 | Consecutive failed reconcile ticks before `stuck_since` flips. |
| `STUCK_THRESHOLD_DURATION` | 5 min | Wall-time since first failure for that peer/collection before `stuck_since` flips. |

`stuck_since` flips when **either** threshold fires. Both are captured by the supervisor and exposed on `ClientPeerStatus`. Backoff between reconcile attempts is **implicit** via the existing `P2P_SUPERVISOR_INTERVAL` tick cadence — no separate exponential schedule, no separate backoff state. The reconcile loop is the retry.

`PairingErrorClass` is a tight enum: `RpcTimeout`, `RpcError`, `RemoteNotFound`, `RemoteUnauthorized`, `LocalError`. The class is what's exposed for visibility; the human-readable error message is logged separately via `tracing`.

### 7.3 Conformance harness scaffold + one passing scenario

Per #155 §3 and the §4.1 action mapping, this spec scopes a conformance harness in `crates/defra-agent/tests/state_machine_conformance.rs` (existing pattern, not a new crate). The harness:

1. **IR:** JSON scenario format whose vocabulary is the §4.1 action mapping table. One JSON action per logical TLA+ action. Example: `{"op": "OperatorWrite", "node": "A", "peer": "B", "collections": ["c1", "c2"]}`.
2. **Orchestration:** spawn two `defra-agent` processes (A and B) with the embedded node + supervisor, drive the scenario, capture per-node DefraDB subscription state and `ClientPeerStatus` snapshots.
3. **Validation:** at every observation point, evaluate the safety invariants from `ReversePairing.tla` against the joint observed state. After a quiescence wait following the last action, evaluate the leads-to convergence target.
4. **Scope at v1:** the IR + the orchestration loop + **one** hand-written scenario — two-peer install/teardown happy path. Counterexample-driven scenario generation deferred to #155's follow-up.

The harness IR derives from §4.1; that's the bridge from TLA+ to Rust.

## 8. v1 scope

The `RemoteP2pAdmin` trait carries all 15 existing HTTP P2P operations (full 1:1 parity with the HTTP surface today, no rename, no semantic change):

| Op | HTTP today | NAC perm (DefraDB) |
|---|---|---|
| `peer_info` | `GET /p2p/info` | `NodeGetP2PPeerInfoPerm` |
| `active_peers` | `GET /p2p/active-peers` | `NodeGetP2PActivePeersPerm` |
| `connect` | `POST /p2p/connect` | `NodeConnectP2PPeerPerm` |
| `list_replicators` | `GET /p2p/replicators` | `NodeListP2PReplicatorPerm` |
| `add_replicator` | `POST /p2p/replicators` | `NodeAddP2PReplicatorPerm` |
| `delete_replicator` | `DELETE /p2p/replicators` | `NodeDeleteP2PReplicatorPerm` |
| `list_p2p_collections` | `GET /p2p/collections` | `NodeListP2PCollectionPerm` |
| `add_p2p_collections` | `POST /p2p/collections` | `NodeAddP2PCollectionPerm` |
| `delete_p2p_collections` | `DELETE /p2p/collections` | `NodeDeleteP2PCollectionPerm` |
| `list_p2p_documents` | `GET /p2p/documents` | `NodeListP2PDocumentPerm` |
| `add_p2p_documents` | `POST /p2p/documents` | `NodeAddP2PDocumentPerm` |
| `delete_p2p_documents` | `DELETE /p2p/documents` | `NodeDeleteP2PDocumentPerm` |
| `sync_documents` | `POST /p2p/documents/sync` | `NodeSyncP2PDocumentsPerm` |
| `sync_collection_versions` | `POST /p2p/collections/sync-versions` | `NodeSyncP2PCollectionVersionsPerm` |
| `sync_branchable_collection` | `POST /p2p/collections/sync-branchable` | `NodeSyncP2PBranchableCollectionPerm` |

The supervisor's reconcile loop in v1 drives **only** the collections + replicators subset (`add_p2p_collections`, `delete_p2p_collections`, `list_p2p_collections`, `add_replicator`, `delete_replicator`, `list_replicators`). This is the surface `pairing.rs` uses today and the surface the TLA+/Lean models cover. The other operations are reachable via the trait but driven from existing call sites (CLI commands, bootstrap, manual UX), not from the reconcile loop. v2 may extend the loop to cover more of the surface when proof artifacts for those operations exist.

`sync_*` operations are reachable via the trait but **explicitly not** in any reconcile loop. They conflate control- and data-plane and want their own verification artifact before being driven automatically.

## 9. Migration

`RemoteP2pAdmin` picks transport per peer based on the peer record. When the peer record carries a GraphQL URL, `HttpRemoteP2pAdmin` is constructed. When the peer record carries only a P2P address and `P2pRemoteP2pAdmin` is available (i.e., the defradb.rs admin channel has shipped), the P2P impl is constructed. Both impls implement the same trait; the supervisor doesn't care.

`pairing.rs` is deprecated. Its caller `configure_local_runtime_pairing` (in `client/core/bootstrap.rs`) moves to call `RemoteP2pAdmin` via the same trait surface — same retry semantics, same idempotence.

Rollout:

1. **Land `RemoteP2pAdmin` + `HttpRemoteP2pAdmin` + supervisor extension behind a feature flag** (`DEFRA_AGENT_PAIRING_RECONCILE`, default off).
2. **Dogfood on desktop** with the flag on; verify reconcile convergence, retry visibility, and that the conformance scenario passes.
3. **Drop the flag** when stable; remove `pairing.rs`.
4. **Add `P2pRemoteP2pAdmin`** when defradb.rs's admin channel is consumable.
5. **HTTP impl stays** for the lifetime of HTTP-supporting deployments; both transports coexist.

Migration does not require coordinated cross-repo releases: defra-agent ships #107 with HTTP-only, and the P2P impl drops in later as a pure extension. The transport-agnostic trait is the boundary.

## 10. Cross-repo coordination

**This spec's deliverable scope (defra-agent):**
- `RemoteP2pAdmin` trait (1 new file in a new `defra-agent-remote-admin` crate or in `defra-p2p-adapter` — implementation-plan choice).
- `HttpRemoteP2pAdmin` impl (1 new file).
- Supervisor extension in `crates/defra-agent-desktop-core/src/client/core/supervisor.rs` (edits to existing).
- `ClientPeerStatus` extension (edits to existing).
- `PeerPairingDesired` schema in `crates/defra-agent-protocol/schemas/`.
- New Lean module `crates/defra-agent/proofs/Proofs/PairingReconcile/*` — mandatory, not optional. The conformance harness IR derives from §4.1's action mapping table; the Lean module proves the supervisor honors the TLA+ `Reconcile(n)` action semantics.
- Conformance harness scaffold + one passing scenario in `crates/defra-agent/tests/state_machine_conformance.rs`.

**defradb.rs (Rust, upstream — not blocked on this spec):**
- #166: Persist gossipsub subscription state on the Rust side to reach parity with Go.
- #168: Audit and tighten persist-before-ack on the existing HTTP `/p2p/*` handler.
- Eventual admin CommChannel implementation. Wire-format design is upstream's — this spec specifies only the contract (operations + semantics).

**defradb (Go, upstream — not blocked on this spec):**
- #168 equivalent on Go's HTTP `/p2p/*` handler.
- Eventual admin CommChannel implementation matching whatever shape defradb.rs picks (or vice versa).
- Identity exchange for admin actor binding — likely reuse of the existing `IdentityProtocol` pattern.

**Cross-repo sequencing:** none required for v1. defra-agent ships #107 with HTTP-only; the upstream P2P transport work proceeds independently. The P2P impl of `RemoteP2pAdmin` drops in as a pure extension when ready.

## 11. Out of scope

- **Authorization implementation.** Tracked under #180.
- **Sync operations in the reconcile loop.** The three `sync_*` ops are reachable via `RemoteP2pAdmin` but are not driven from the supervisor's desired-vs-actual loop. They want their own verification artifact before being automated.
- **Defradb.rs / defradb admin channel wire-format design.** That's upstream's design problem. This spec specifies only the contract.
- **Multi-peer N-way fanout.** The TLA+ model is two-peer. This spec inherits the two-peer scope. N-peer convergence is a future verification extension.
- **Foreground cross-deployment subagent completion.** Explicit non-goal in #176. Future R5+ work.
- **Counterexample-driven scenario generation for the conformance harness.** v1 ships one hand-written scenario; counterexample translation tooling is a #155 follow-up.
- **Durable retry surface (#170 v2).** v1 is in-memory; durable resilience across restarts is a follow-up gated on #166.
- **R5 cross-deployment subagent worker.** This spec produces the substrate; R5 consumes it. R5 is a separate implementation track. **R5 status without #180 closed:** R5 functionally ships under a *trusted-fleet* trust model (closed-source single-org), because the projection logic itself is independent of who installed the substrate. R5 is **not** appropriate for multi-tenant deployment until #180 closes — the proofs' NAC-bound-auth structural assumption isn't honored otherwise.

## 12. Open questions

- **Crate placement for `RemoteP2pAdmin`.** Either a new `defra-agent-remote-admin` crate (clean dependency boundary; reusable from CLI and desktop) or grown into the existing `defra-p2p-adapter` (single trait family for both local and remote P2P concerns). The implementation plan picks before code lands.
- **`PeerPairingDesired` derivation vs. explicit collection — locked to explicit.** Open follow-up: do existing pairing UI/CLI surfaces write the doc directly, or is there a service layer that translates the existing flow into doc writes? Implementation-plan decision.
- **Retry-parameter defaults.** §7.2 proposes values; the implementation plan revisits them against real telemetry once dogfooding starts.
- **Lean module organization.** §10 names `Proofs/PairingReconcile/{State,Transition,Executable,Convergence}.lean` following the `RuntimeReconcile/` pattern. The Lean module's organization may need to deviate based on what shares structure with existing modules.
- **Conformance harness orchestration mechanics.** §7.3 lists the IR + the loop shape but not the exact process-spawn mechanism. Backbone-style primitives exist in defradb-land; the implementation plan picks between reusing those and writing inline tokio orchestration.
- **Stuck-threshold defaults vs. real recovery times.** `STUCK_THRESHOLD_ATTEMPTS = 6` and `STUCK_THRESHOLD_DURATION = 5 min` are proposals. Real-world recovery times from network blips, peer restarts, and partition healing are unknown; values are revisited after dogfood telemetry.
