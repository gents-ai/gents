# P2P convergence diagnosis: #630, #696, and #798

Qualified with Gents based on `7a992d59` and DefraDB PR
[#1502](https://github.com/sourcenetwork/defradb.rs/pull/1502) at `c340eaf2`.

## Conclusion

The three issues are related mechanisms, not one defect:

- #630 was sender-side amplification: one logical update became an ordered,
  full-DAG PushLog for every peer. Fan-in made the hub's bounded push workers
  the visible bottleneck.
- #696 combined that sender amplification with collection-head identity,
  gossip-direction, admission, and missing-link recovery defects. A fresh mesh
  made the interaction look like one runaway pending-DAG failure.
- #798 was an interaction between Gents control-plane amplification and the
  remaining DefraDB ownership defects. Gents produced avoidable physical
  documents and unchanged lease heads; DefraDB admitted overlapping sender,
  receiver, recovery, and merge owners for the resulting DAGs.

Transport symptoms were consequences. A PushLog timeout, empty selective CAR,
provider rotation, pending-DAG registration, or retry was not independently the
root cause. They followed either excess application document production or an
unfulfilled/duplicated sync ownership obligation.

This was not a CRDT merge-semantics bug. The transaction conflicts observed in
`pending_store.remove` were ordinary OCC conflicts between concurrent owners of
the same mutable sync metadata. Duplicate `AgentNetwork` rows were distinct
DefraDB documents created by Gents for the same signed logical record, not two
CRDT values incorrectly merged into one document.

## Amplification boundary

The final causal split is:

1. Gents created a large but bounded network-control topology.
2. Each join also recreated the issuer's signed `AgentNetwork` and
   `NetworkMembership` locally, assigning the same logical records new physical
   document identities.
3. One-second qualification intervals rewrote unchanged `PeerEndpoint` and
   `PeerRegistry` leases every second even though the freshness window was five
   minutes.
4. The old DefraDB sender expanded every new head into per-peer full-DAG work.
5. Receiver recovery and terminal cleanup also acted on the same roots, causing
   CAR/provider storms and pending-store contention.

DefraDB #1502 removes steps 4 and 5 by transferring one current-head hint into
one durable receiver obligation, pacing rooted CAR recovery, qualifying
providers, and serializing the P2P merge owner. This Gents change removes the
avoidable production in steps 2 and 3 and suppresses a repeated no-match
`BearerPairingReady` delete.

## Gents changes

- Join is layered using the pairing model landed in #1156. It writes the
  `network-control` base pairing plus the requested data-plane pairing in one
  mutation. The issuer remains the only writer of the signed network root and
  membership grant; the joiner receives those exact documents through the
  control replicator.
- `PeerEndpoint` and `PeerRegistry` publish on initial state, binding/status
  change, or lease renewal. Failed writes remain due, and registry addresses are
  sorted and deduplicated before comparison.
- `BearerPairingReady` deletion first proves an issuer-and-peer row exists, so a
  settled sweep does not open an empty delete mutation.
- Successful GraphQL mutations log their operation and affected-document count
  at debug level, making remaining write sources attributable.
- The release fence reads DefraDB's effective replicator collection IDs instead
  of treating desired/applied document fields as installed scope.
- The release harness exposes stage-3 queue, marker, CAR, provider, admission,
  and terminal counters; it requires three stable quiet samples before and
  after coordinator restart.

The mutation-site audit found no other settled write clock in this bundle.
Desired/applied pairing, bearer claims, network derivation, reciprocal intent,
directory projection, and runtime status already converge to write-free
fixpoints when their inputs are unchanged.

## Reproduction and acceptance

```bash
GENTS_RELEASE_ACCEPTANCE=1 \
GENTS_RELEASE_CONTROL_ONLY=1 \
GENTS_RELEASE_PRESERVE_STORES=1 \
RUST_MIN_STACK=8388608 \
cargo test -p gents-cli --features live-e2e --test cli_live_suite \
  cli_fleet_delegation_live::nineteen_process_release_acceptance_live -- \
  --ignored --nocapture --test-threads=1
```

The topology is 19 fresh stores: one coordinator and 18 spokes, 18 signed
invite/join operations, and the derived network-control mesh. Admission remains
bounded at 1,000 in-memory pending DAGs, 4,000 persisted pending DAGs, eight
push workers, and four recovery workers. The test accelerates observation to
one second while unchanged leases renew every 100 seconds.

| Candidate | Result | Runtime | Capacity shed | Fetch exhausted | Quarantine | Missing effective scope | Lease writes |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| pinned baseline | failed | 145.01 s | 14,775 warnings | not observed | not observed | 1 `AgentNetwork` edge | not counted |
| stage 3 before Gents suppression | failed | 434.88 s | 0 | 0 | 0 | 1 edge | 6,889 endpoint / 6,975 registry |
| stage 3 after lease suppression | failed upstream recovery | 387.45 s | 0 | 60 | 0 | 0 | 76 endpoint / 76 registry |
| `c340eaf2` + final Gents, run 1 | passed | 382.13 s | 0 | 0 | 0 | 0 | at most 16 of each per node |
| `c340eaf2` + final Gents, run 2 | passed | 388.73 s | 0 | 0 | 0 | 0 | at most 16 of each per node |

Both passing runs started from new temporary roots and crossed coordinator
restart/remesh. At every quiet fence they had empty in-memory and persisted
pending-DAG sets, no durable retry markers, no pending resync, no
non-authoritative broadcast tasks, no queued or active push jobs, and stable
progress counters. The log gate observed no empty selective CAR response,
unparseable pushed block, or rejected gossip direction.

## Formal boundary

No new Lean or TLA transition is introduced here. The control/data layering
used by join is the executable path already modeled and proved in
`Proofs/PairingReconcile/Layering.lean` by merged PR #1156. The remaining Gents
changes affect write timing, no-match plumbing, diagnostics, and acceptance
observation without changing freshness thresholds, legal pairing transitions,
admission bounds, retry dispositions, or convergence claims.

DefraDB #1502 carries the formal ownership change: TLA models the durable
receiver obligation, provider qualification, bounded retry/admission, and
single merge owner before conformance and Rust. Its non-gating follow-ups
#1537, #1538, and #1539 remain separate. Gents #977, #1036, #1049, and #1122
remain ordered follow-ons and were not absorbed into this bundle.
