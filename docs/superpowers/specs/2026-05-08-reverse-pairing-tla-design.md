# Reverse-Pairing Subscription Convergence — TLA+ Spec Design

**Status:** Design
**Date:** 2026-05-08
**Tracks:** issue #155 (cross-boundary verification strategy); sibling to issue #107 (P2P-only subscription management)
**Scope:** TLA+ spec design for the control-plane convergence property. Implementation (the actual TLA+ source + model checker run) tracked in a follow-on plan.

## Background

Per #155, defra-agent's per-node Lean proofs cover twelve lifecycle areas and ship Cedar-style differential conformance against Rust. Everything that crosses a node boundary — P2P data sharing, subscription/replicator management, gossip propagation, client-side projection — sits in the deviations file. There is no formal model.

The first concrete cross-node surface to model is **reverse-pairing of subscriptions and replicators between two peers**. This is the loudest current source of bugs in production multi-node runs and is the surface most directly affected by #107's planned migration of P2P management from HTTP to a P2P admin RPC.

This spec fixes the abstract TLA+ model: state, actions, properties, and the modeling assumptions that make the property provable. It also enumerates derived requirements that fell out of the analysis — concrete inputs back to #107 and to `defradb.rs`.

## Investigation findings

Before fixing model assumptions, we audited the actual delivery semantics of both DefraDB implementations against the question "what does the transport actually guarantee?". Both Rust and Go were investigated; findings agreed on the keystone facts.

**Data plane.** Both implementations use libp2p with two distinct lanes:
- **Gossipsub** for change notifications: lossy, unordered, duplicates possible, no application-layer retries.
- **Request-response streams** for replication pushlog: reliable per attempt, ordered within a stream, with timeout.

Reconnect/catch-up differs: Rust does a full re-replication on replicator install (BranchableSync + CarFetch from collection root CID); Go uses a persisted async retry queue keyed by `peerID/docID`.

**Control plane today.** HTTP only on both implementations. This is the implementation gap #107 is closing.

**Future P2P admin RPC.** The Go `comm_channel` pattern (`internal/db/p2p/protocol/comm_channel.go`) is the proposed wire shape and the Rust port aims for parity:

- UUID-tagged request/response over libp2p stream
- Synchronous send; caller blocks on response or a 10s timeout
- Authenticated via signed message
- No automatic retry at the protocol layer
- No idempotency marker — caller is responsible for idempotent operations

**Persistence.** Replicator config and status are persisted on both sides. Subscription metadata is persisted in Go (`systemstore` marker bytes) but in-memory only in Rust — almost certainly a Rust gap, not a design choice. In-flight admin requests evaporate on crash on both sides.

## Goals

- Specify a TLA+ model whose execution traces refine the actual reverse-pairing flow under #107's planned wire protocol.
- Prove **mutual convergence**: safety (no durable phantom or orphan state on either side) plus liveness (under finite network instability and finite crashes, both sides eventually agree).
- Surface modeling assumptions as explicit obligations on the implementation: handler idempotency, persist-before-ack, persisted desired-state intent.
- Enumerate the derived requirements that must hold for the proof to apply to the running system, so each becomes an actionable input to another issue or PR.

## Non-goals

1. **Data-plane convergence.** Once subscriptions are installed, "do all docs eventually replicate" is a separate property, modeled separately. Likely a follow-on TLA+ spec.
2. **Authorization correctness.** "Remote only installs subscriptions for authorized actors" is a structural invariant — better proven in Lean against the admin RPC handler than in TLA+.
3. **Concurrent operator races on desired state.** Assume a single-writer desired-state document per node.
4. **Client-side UI projection.** Modeled separately. Lean already covers turn projection.
5. **Multi-peer fanout.** Two-peer reverse-pairing first; N-peer is a future extension.
6. **The HTTP control plane that exists today.** Model targets the post-#107 P2P admin RPC.

## Model

### State (per node `n`)

| Field | Persistence | Meaning |
|---|---|---|
| `desired[n]` | persisted | operator-set: `Peer → Set[Collection]` — for each peer `p`, the set of collections `n` wants `p` to push to it |
| `replicator[n]` | persisted | `n`'s local replicator entries: `Peer → Set[Collection]` — the set of collections `n` actively pushes to that peer |
| `in_flight[n]` | in-memory | RPCs `n` has sent but not yet received a response on |
| `pending_inbound[n]` | in-memory | RPCs `n` is currently processing |

Replicators are physically asymmetric: each replicator entry lives on a single side and pushes one direction. Reverse-pairing requires two replicators (one on each side, pushing opposite directions). The model captures this directly: `replicator[A][B]` and `replicator[B][A]` are independent state.

The model treats `replicator[p]` as readable from `n`'s reconcile loop. In implementation, `n` only learns `replicator[p][n]` via acks and DefraDB subscription; the local cache is a refinement of the abstract read and is not modeled here. Cache lag delays decisions but does not make them incorrect, since the underlying handler is idempotent.

### Network state

```
messages : Multiset[Message]
Message = ⟨src, dst, rpc⟩
```

The network may drop, reorder, or duplicate messages, but only finitely many drops occur between any two infinite-progress points (eventual delivery under fairness).

### RPC kinds

```
RPC      = ⟨id : RPCId, kind : RPCKind, src : Node, tgt : Node, collection : Collection⟩
RPCKind  = Install | Teardown | Ack ⟨of : RPCId⟩
```

An `Install` RPC says "src wants tgt to install a replicator pushing this collection from tgt back to src." A `Teardown` RPC says "src wants tgt to remove that replicator." `Ack` carries the originating RPC's id; the receiver of an ack matches it back to its `in_flight` entry. `(src, tgt, collection)` identifies the logical install relationship; `id` is unique per attempt.

### Actions

| Action | Effect |
|---|---|
| `OperatorWrite(n, p, c)` | Add or remove `(p, c)` from `desired[n][p]`. Persisted. |
| `Reconcile(n)` | For each `(p, c) ∈ desired[n][p] \ replicator[p][n]` with no matching pending Install in `in_flight[n]`, emit `InstallRPC(n→p, c)` and add to `in_flight[n]`. For each `(p, c) ∈ replicator[p][n] \ desired[n][p]` with no matching pending Teardown, emit `TeardownRPC(n→p, c)` and add to `in_flight[n]`. |
| `Send(msg)` | `messages := messages ∪ {msg}`. |
| `Deliver(msg)` | Remove `msg` from `messages`; add to `pending_inbound[msg.dst]`. |
| `Drop(msg)` | Remove `msg` from `messages`. Bounded by fairness. |
| `Process(recv, rpc)` | Run handler. For `Install`: persist `(rpc.src, rpc.collection) ∈ replicator[recv][rpc.src]`. For `Teardown`: persist `(rpc.src, rpc.collection) ∉ replicator[recv][rpc.src]`. **Persist before emitting the corresponding `AckRPC(recv→rpc.src, rpc.id)`.** Remove from `pending_inbound[recv]`. |
| `ReceiveAck(n, ack)` | Match `ack.of` to an entry in `in_flight[n]`. Remove that entry. (No persisted state change on `n` from receiving the ack — the install/teardown happened on the peer's side.) |
| `Timeout(n, rpc)` | Remove `rpc` from `in_flight[n]`. Next `Reconcile` re-evaluates against current `replicator[p]` and re-emits if still needed. No state change beyond `in_flight`. |
| `Crash(n)` | Clear `in_flight[n]`, `pending_inbound[n]`. Preserve `desired[n]`, `replicator[n]`. |
| `Recover(n)` | No-op on persisted state; next `Reconcile` re-runs from `desired` against the current `replicator[p]` view. |

### Modeling assumptions

These are the obligations that must hold on the real system for the proof to apply.

- **Handler idempotency.** `Process(recv, install_rpc)` invoked for a `(rpc.src, rpc.collection)` already present in `replicator[recv][rpc.src]` is a state no-op and emits an ack. Symmetric for `Teardown`. Discharged in Lean per-handler.
- **Persist-before-ack.** `Process` writes to persisted `replicator[recv]` strictly before emitting the ack. Receiver-side crash mid-handler leaves `replicator` either fully updated or fully untouched — never half.
- **Persisted desired and replicator state.** `desired[n]` and `replicator[n]` both survive crash. Operator changes to `desired` flow through defra-agent's existing apply/reconcile path.
- **Eventually-healthy network.** Drops are finite between any two infinite-progress points. Equivalent to weak-fairness on `Deliver`.
- **Finite crashes.** Each node crashes at most finitely many times in any execution.

## Properties

The model splits its claims into pure safety, pure liveness, and a supporting inductive invariant. Pure safety is what holds in every reachable state. Pure liveness is what eventually holds given fairness. The supporting invariant is a qualified state property that drives the liveness proof.

### Safety: structural and provenance invariants

State changes are mediated only by the documented actions. As pure safety properties (witnessed by a finite prefix if violated):

- `replicator[p][n]` changes only via `Process(p, install_rpc)` (additions where `rpc.src = n`) or `Process(p, teardown_rpc)` (removals where `rpc.src = n`).
- `desired[n][p]` changes only via `OperatorWrite(n, p, ...)`.
- `in_flight[n]` is populated only by `Reconcile(n)`, drained by `ReceiveAck(n, ...)` or `Timeout(n, ...)`, and cleared by `Crash(n)`.
- `messages` is populated only by `Send(...)` and drained by `Deliver(...)` or `Drop(...)`.

Provenance:

```
∀ trace τ, ∀ state s ∈ τ, ∀ n, p, c :
  (n, c) ∈ replicator[p][n]_s
  ⇒ ∃ s' ≺ s :
      (p, c) ∈ desired[n][p]_{s'}
      ∧ Install RPC for (n, p, c) was Processed at s'
      ∧ no Teardown for (n, p, c) was Processed in (s', s]
```

Read: every `replicator` entry traces back to an operator-initiated `desired` request that produced an `Install` RPC that was processed, with no subsequent `Teardown` having been processed since. No spontaneous installs; no installs without prior operator intent.

### Liveness: leads-to convergence

Under fairness on `Deliver`, `Process`, and `Reconcile`, plus the modeling assumptions:

```
∀ n, p, c : (p, c) ∈ desired[n][p] ⤳ (n, c) ∈ replicator[p][n]
∀ n, p, c : (p, c) ∉ desired[n][p] ⤳ (n, c) ∉ replicator[p][n]
```

Read: any state where `desired[n][p]` and `replicator[p][n]` disagree leads-to a state where they agree. Both install and teardown convergence are covered.

For reverse-pairing, `desired[A][B] ⊇ S ∧ desired[B][A] ⊇ S` leads-to `replicator[B][A] ⊇ S ∧ replicator[A][B] ⊇ S` — both directions installed.

### Supporting invariant: in-flight justification (post-Reconcile states)

The following inductive invariant supports the leads-to liveness proof. It does *not* hold in every reachable state — specifically, between `OperatorWrite(n, p, c)` and the next `Reconcile(n)` firing for that pair, the disagreement exists but no RPC has been emitted yet. The invariant holds in the closure of states reachable after `Reconcile(n)` has fired since the last `OperatorWrite(n, p, _)`:

```
∀ n, p, c :
  (p, c) ∈ desired[n][p] ∧ (n, c) ∉ replicator[p][n]
  ⇒ ∃ rpc :
      rpc.kind = Install ∧ rpc.src = n ∧ rpc.tgt = p ∧ rpc.collection = c
      ∧ rpc ∈ in_flight[n] ∪ messages ∪ pending_inbound[p]
```

```
∀ n, p, c :
  (n, c) ∈ replicator[p][n] ∧ (p, c) ∉ desired[n][p]
  ⇒ ∃ rpc :
      rpc.kind = Teardown ∧ rpc.src = n ∧ rpc.tgt = p ∧ rpc.collection = c
      ∧ rpc ∈ in_flight[n] ∪ messages ∪ pending_inbound[p]
```

Read: in the steady state — that is, after `Reconcile` has had a chance to react to any operator change — every disagreement is matched by a reconciling RPC somewhere in the system: pending in `n`'s in-flight set, in transit, or being processed by `p`. Together with fairness on `Deliver`, `Process`, and `Reconcile`, plus idempotent handlers and persist-before-ack, this drives the leads-to liveness above.

The window between `OperatorWrite` and the next `Reconcile` is the only reachable state where disagreement exists without a corresponding in-flight RPC. It is bounded by `Reconcile`'s fairness assumption — that is, finite. The TLA+ source author may want to model this window explicitly via a phase variable, or absorb it by treating `OperatorWrite` and the immediately-following `Reconcile` as a single composite action; the design here records this as an open formulation choice rather than committing to one option.

At quiescence (no in-flight, no in-transit, no in-process RPCs, no enabled `Reconcile`), the supporting invariant collapses to `desired[n][p] = {c : (n, c) ∈ replicator[p][n]}` for every pair — `desired` and `replicator` must agree across the system.

## Boundary discipline: timeouts

Per #155 §4, every timeout in the spec carries an explicit liveness justification:

| Timeout | Justification |
|---|---|
| `Timeout(n, rpc)` | Liveness only. Caller cannot distinguish "RPC never arrived" from "RPC processed but ack lost." Both cases recover via the next `Reconcile`. The action removes the RPC from `in_flight` but makes no other state change. No safety claim rides through this timeout. |

If a future revision adds a timeout whose effect mutates `replicator` or `desired`, that revision is a deviation and the safety proof must be re-derived from the updated transition relation.

## Derived requirements

These fell out of the analysis. Each is an actionable input to another issue or PR; they are the "this exercise pays for itself" outputs.

1. **Persist-before-ack on receivers.** Every state-mutating admin RPC handler must persist its state change strictly before emitting the ack. Without this, a receiver-side crash mid-handler leaves a half-applied subscription that violates the safety property. → input to #107 (handler implementation).

2. **Idempotent install/teardown handlers.** Each handler must produce the same final persisted state regardless of how many times it is invoked with the same logical RPC. → Lean obligation, sibling to the TLA+ spec. Each handler ships an idempotency lemma.

3. **Persistence parity for collection-subscription state in Rust.** This model covers replicator entries, which Rust persists on both sides. The investigation also surfaced a related gap: Rust's gossipsub-level "subscribed collection IDs" list is in-memory only, asymmetric with Go's persisted form. The full reverse-pairing user-experience depends on both replicators (push) and gossipsub subscriptions (change-notification) — extending this model to cover the gossipsub layer requires that state to be persisted on the Rust side first. → input to a `defradb.rs` follow-on issue.

4. **Stuck-retry visibility.** Unbounded caller retry cleanly satisfies the timeouts-for-liveness-only principle, but it creates an operator visibility gap: a caller retrying for hours against a permanently-gone peer looks the same as one retrying for seconds against a temporarily-partitioned peer. → input to a future operator-surfaces issue. Surface via a runtime-visible "stuck retry" indicator carrying elapsed-time, attempt count, and last-error metadata.

## Harness

The TLA+ proof establishes correctness of the abstract protocol. Differential conformance via a multi-node harness checks that the running system refines the model. Per #155 §3, the same shape as the Lean → JSON conformance pattern applies; the source is TLA+ traces rather than Lean.

For reverse-pairing convergence, the harness needs:

1. **Process orchestration.** Spawn two `defra-agent` binaries with distinct keypairs, connect them over real libp2p, let them run as full processes — not in-process mocks. Backbone-style primitives already exist on the defradb side; reuse rather than rewrite.

2. **Driving channel — TLA+ action → real operation.**
   - `OperatorWrite` → `defra-agent-cli config apply` (transactional after #56)
   - `Reconcile` → no explicit drive; reconcile runs continuously inside each agent
   - `Crash(n)` → `SIGKILL` of the process; `Recover(n)` → restart from persisted state
   - `Drop` / partition → process-level network blackhole between the two PIDs (e.g., iptables, network namespace, or a pluggable proxy)
   The TLA+ scenario format need not be 1:1 with these — it can be higher-level — but the harness must implement a deterministic translation.

3. **Observation channel.** Per-node DefraDB subscription on the relevant collections plus polling on derived live-state docs. Captures `desired[n]` and `replicator[n]` changes per node as a timestamped event stream. The model's persisted fields belong on the daemon-visibility boundary that the existing per-node Lean model already defines (`crates/defra-agent/proofs/`); exposing them is part of fitting this work into that pattern, not a separate observability requirement.

4. **Conformance check.** At every observation point, evaluate the safety invariant against the joint observed state. After a quiescence wait following the last action, evaluate the liveness target. A safety violation observed in the harness when TLC says safety holds is a refinement failure — either a model-implementation gap or a harness translation bug; both are interesting findings.

5. **Scenario format.** A JSON file enumerating an ordered action sequence (operator writes, network events, crashes, RPC delivery delays) with optional timing constraints. TLC counterexamples serialize into this format; passing scenarios are also random-walks of the model. The harness consumes and executes deterministically.

The harness is sequenced after the TLA+ source and a first model-checker run, but the spec for what the harness consumes and produces is part of the design closure for this property.

## Open questions

- **Network model granularity.** The action sketch has `Send`/`Deliver`/`Drop`. Real libp2p streams give in-order delivery per stream and clean failure on disconnect — strictly stronger than the sketch. Whether to model that is a tradeoff between fidelity and proof tractability. Start abstract; refine if it hides bugs.
- **Concurrent reconciles.** If `Reconcile(n)` runs while a previous reconcile's RPCs are in flight, the model must avoid double-emitting. The "no matching pending Install / Teardown in `in_flight[n]`" guards in the action definition handle this; verify the guards compose correctly under interleaved reconciles and survive the crash/recover transition (where `in_flight` is cleared but persisted state is not).
- **Where the TLA+ source lives.** `crates/defra-agent/proofs/` alongside Lean, or a sibling `proofs/tla/`. #155 lists this as an open question; this spec defers.
- **Whether to model batched multi-collection RPCs.** Real admin RPCs may install multiple collections in one call. The sketch is per-collection; batching is a refinement step that should not change the safety property.
- **Tooling.** TLC vs. Apalache vs. PlusCal frontend. Decide before writing the spec.

## Implementation steps (informational; not part of this spec)

1. Decide TLA+ tooling and directory placement.
2. Translate the model above into TLA+ syntax.
3. Run TLC under bounded parameters (2 nodes, 2 collections, bounded crashes) for the safety property.
4. Add fairness annotations and check liveness.
5. Capture failing trace counterexamples (if any) and translate them into JSON scenarios for the planned multi-node harness (#155 §3).
6. File the four derived requirements as separate issues / PRs.
7. Discharge derived requirement #2 (handler idempotency) in Lean as a sibling proof.
