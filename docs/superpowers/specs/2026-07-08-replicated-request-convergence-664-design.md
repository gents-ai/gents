# Replicated request-state convergence (#664)

## Problem

Under `subagent-host` replication, an `AgentRequest` document is replicated onto
non-owning peers. The #661 incident export showed the owner (Amy) reaching
terminal `failed` while all 14 peer replicas stayed at the owner's earlier
`claimed`/`processing` state — a permanent divergence with no mechanism on a
running peer to converge it.

Two facts, both established from code + the incident evidence:

- **Safety already holds.** No peer ever claims or processes a foreign request:
  `watcher/query.rs` filters `agent_did: {_eq: self.agent_did}` on both claim
  seams (`try_fetch_request`, `pending_requests`). Every peer replica carried the
  owner's DID with `claimed_by=null`; both enabled legs reported
  `ignored_foreign_processing_count=2` (a read-only counter, no mutation).
- **Liveness does not.** The owner's terminal delta never converged to the
  replicas. There is no per-doc anti-entropy re-drive on a running peer
  (defradb.rs#1074: `get_replicators` reads persisted, not live), recovery is
  owner-scoped + startup-only (`recover_stuck_requests`), and `p2p_reconcile` is
  topology-only (keyed on collection names, not doc state).

#663 removed the *trigger* for the observed signature (a successful request
reaches `completed` and replicates normally). This issue closes the underlying
**convergence gap**, which is trigger-independent: any future mid-state
failure / crash / partition that leaves a foreign replica non-terminal
reproduces it.

## Non-goals

- The ~98% CPU / HTTP wedge (cause undetermined — the wedge window is unlogged).
  Separate operational + observability follow-up on #664; not this PR.
- Peer-side writes to foreign documents (rejected — see Mechanism).

## Model (design artifact, TLC-checked locally — CI does not run TLC)

`crates/defra-agent/proofs/tla/ReplicatedRequestConvergence.tla`, reusing
`SubagentCompletion.tla`'s gossip + fairness scaffold (`messages`,
`pendingInbound`, `Deliver`/`Drop`, `Crash*`, `WF` fairness, `~>` leadsto).

Shape: one request doc; an `owner` node and a set of replica-holder peers;
`reqState[node]`; owner-only lifecycle `Transition`; a **re-emittable** terminal
delta (`EmitTerminalDelta`); bounded `DeliverDelta`/`DropDelta`;
`PersistDeltaOnPeer`; `CrashOwner`/`CrashPeer`.

Two properties:

- **SAFETY `SingleClaimer`** — only `owner(r)` ever transitions `r`; no peer
  claims a foreign replica. (Fences the existing `agent_did` filter.)
- **LIVENESS `TerminalConverges`** — owner-terminal `~>` every replica-holder
  reflects terminal. Holds only with owner re-emit + `WF` fairness on delivery.

Two configs (red-then-green at the model level):

- `MCReplicatedRequestConvergence.cfg` — re-emit enabled + fairness → both
  properties hold.
- `MCReplicatedRequestConvergenceStuck.cfg` — a diagnostic with the re-emit
  action disabled (or fairness dropped), reproducing the reachable stuck state
  (owner=terminal, replica=non-terminal, no enabled fixing action) as a
  `TerminalConverges` violation — the counterexample that names the fix.

Register in `tla/README.md` alongside the other specs.

## Mechanism: owner re-drive, passive peers

Chosen over active peer-side projection because:

1. Peers are already safe and correct to be passive — a stale foreign replica is
   harmless to their operation (they ignore it); it only affects read-consistency.
2. Peers writing/projecting foreign documents fights the ACL model (the DID is
   the permission boundary; documents are owner-controlled).
3. Re-push on (re)pair / replicator re-assert is the existing DefraDB-native
   anti-entropy path.

The owner, on a periodic reconcile, **re-drives the terminal state** for
recently-terminalized requests it owns, forcing the terminal delta through the
normal PushLog path rather than depending on a one-shot delivery that can drop.
Bounded (recent window, capped re-emits) — mirrors the model's bounded re-emit +
fairness.

**Binding to be confirmed in Phase 0** (does a same-value re-write produce a new
DefraDB delta, or does the CRDT no-op it?):
- if a re-write produces a delta → periodic idempotent terminal re-assert;
- if it no-ops → replicator re-assert / head re-push (the "push on pair history"
  path) or a monotonic `convergence_seq` bump.
The model is invariant to the binding (it abstracts "owner re-emits terminal
delta"); only the Rust action differs.

## Rust changes (this PR — full mechanism)

1. **Recovery drift fix** — `recover_stuck_requests` sweeps `status=processing`
   only; Lean `requestRecoveryStale` sweeps `claimed ∨ processing`. Align the
   Rust sweep to the model.
2. **Owner re-drive reconcile** — the periodic terminal re-assert per the Phase-0
   binding, owner-scoped, bounded.
3. **Conformance test** — `tests/conformance/replicated_request_convergence.rs`
   mirroring the model: (a) `SingleClaimer` — a foreign-DID replica is never
   claimable by the watcher; (b) `TerminalConverges` — the re-drive re-emits the
   owner's terminal for an unconverged replica. Registered in the conformance
   module (and the structure fence if a Lean model is added).

## Adversarial-review outcomes (folded in)

An adversarial verification pass (liveness + safety + Rust binding, each
independently re-running TLC and the cargo gate) accepted the liveness proof and
the Rust binding, and surfaced two safety must-fixes, both addressed here:

1. **`SingleClaimer` was vacuous.** With `Delta.value : TerminalKind`, a peer in
   `Claimed`/`Processing` was type-impossible, so the invariant could never be
   falsified — its green run proved nothing. Added an `AllowPeerClaim`-gated
   `PeerClaimsForeign` action and a third config
   (`MCReplicatedRequestConvergencePeerClaim`) in which `SingleClaimer` is
   reachably VIOLATED, mirroring the `Reemit` red/green that makes
   `TerminalConverges` load-bearing.
2. **Seam-completeness gap.** There is no write-ACL on `AgentRequest`
   (`agent_did` is `@immutable` so a peer cannot *steal* a request, but the DB
   does not block a peer writing a foreign replica's mutable fields). Two
   `queue.rs` seams (`reconcile_coalesced_pending_request`,
   `drain_pending_session_requests_where`) transitioned rows scoped by
   `session_id` only, safe merely under an unenforced single-DID-per-session
   invariant. Added an `agent_did == self` conjunct to both the candidate
   queries and the mutations (belt-and-suspenders; the fence is now local, not
   an implicit invariant), threaded through all callers, with two conformance
   tests proving a foreign replica is never superseded/drained.

The binding-soundness question ("does a same-value re-write no-op?") was resolved
in Phase 0 against defradb.rs at the pinned rev `63c0be62`: it does **not**
no-op — `modified_fields` is the patch's key set (not an old-vs-new diff), the
write path has no value-equality short-circuit, and every write takes
`priority = max+1`, so a same-value terminal re-assert is a genuine
higher-priority CRDT delta that flows through the normal PushLog path and a
lagging replica accepts by LWW.

## Validation

Model: `MCReplicatedRequestConvergence` green; `MCReplicatedRequestConvergenceStuck`
(liveness) and `MCReplicatedRequestConvergencePeerClaim` (safety) expected
violations. Code: `cargo test -p defra-agent` (full) + `cargo check --workspace
--all-targets`. Every conformance fence verified red-then-green.

## Known follow-ups (not blocking)

- Wedge-window observability + `finalize_request_failure` terminal-write
  robustness (the ~98% CPU incident cause is undetermined; unlogged window) —
  tracked on #664.
- `created_at DESC` is a proxy for terminalization time; under very high
  request-creation rate a long-running task's recently-terminalized row could
  fall outside the top-`N` window. Eliminable with a `terminalized_at` /
  `convergence_seq` field (deliberately not added now to avoid schema churn).
- Remaining raw `agent_did` interpolation in the sibling `recover_stuck_*`
  functions this PR does not touch (pre-existing; `recover_stuck_requests` is
  fixed here since it is modified).

Refs #664, #661, #663, #630. defradb.rs#1074.
