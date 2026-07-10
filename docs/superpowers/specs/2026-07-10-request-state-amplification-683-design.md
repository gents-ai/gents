# Request-state replication scope and amplification (#683)

## Decision

`AgentRequest` replication is scoped to the two principals that are parties to
one cross-deployment request. It is not an agent-wide backup channel and it is
not a fleet broadcast channel.

For a coordinator DID `C` and host DID `H`:

- The coordinator leg sends `AgentToolCall` bridges where
  `spawn_target_did == H`. It sends no coordinator-owned `AgentRequest` rows.
  A coordinator request is not specific to one host: one request may delegate
  to zero, one, or several hosts, so `agent_did == C` is not a sound pair key.
- The host materializes the child request locally and stamps its immutable
  `requester_did = C` routing key at create time.
- The host leg sends `AgentRequest` rows where `requester_did == C`. A host
  paired with several coordinators therefore returns each child request only to
  the coordinator that requested it.
- Non-request collection rules are unchanged by #683.

This matches the product boundary: P2P pairing establishes a transport
relationship; a scope template selects the application documents needed by
that relationship.

## Why the old filter amplified

The existing filtered-replication machinery is active, but the coordinator
request predicate is `AgentRequest.agent_did == local_did`. The reconciler
installs that same local-agent predicate once for every host pairing. On a
coordinator with 16 hosts, every coordinator-owned request matches all 16
replicators. The filter is agent-scoped, not pair-scoped.

The host leg has the symmetric problem for requests: `agent_did == host` sends
the host's whole request slice to each coordinator pairing. The immutable
`requester_did` discriminator closes both request-state leaks without changing
the other conversation collections in this issue.

## Bridge self-sufficiency

The remote host currently reads the replicated parent request only to recover
the parent DID/depth and to observe cancellation races. After the scope change:

- the bridge's immutable `agent_did` identifies the coordinator;
- normalized bridge args carry `parent_subagent_depth` alongside the already
  resolved target DID/behavior;
- the targeted bridge lifecycle and cancel-cascade fields remain the remote
  cancellation signal.

The local/same-node path keeps its parent-row cross-reference check. The
trusted paired-peer path treats the targeted, owner-authored bridge as the
durable cross-deployment parent edge and does not require a second copy of the
parent request on the host.

## Convergence interaction (#667/#681)

The owner-only safety rule is unchanged: only `AgentRequest.agent_did`'s owner
may drive lifecycle state. Terminal convergence is quantified over authorized
request-party replica holders, not every paired fleet node.

Host-owned cross-deployment children retain the bounded three-attempt terminal
re-drive and reconnect replay to their `requester_did` coordinator. Ordinary
local requests and coordinator parent requests have no remote request-state
consumer, so terminal re-drive skips them. This suppresses convergence writes
that cannot repair a replica while retaining the #667/#681 mechanism wherever
a replica actually exists.

`requester_did` is `@immutable`, as required by DefraDB filtered replication:
a document cannot drift into or out of a peer scope after creation. DefraDB
rejects null-to-value updates on newly added immutable fields, so legacy rows
cannot be safely backfilled. Upgraded stores leave those rows unkeyed and
excluded from the new request route; new cross-deployment children are keyed at
creation. Existing targeted bridge rows remain the recovery/materialization
source for in-flight work.

## Rollout constraints

This scope change is intentionally one-way compatible during a rolling
upgrade. An old coordinator still sends a parent request that a new host can
use as a legacy fallback. A new coordinator sends only the targeted bridge,
which an old host cannot materialize without the former parent row. Operators
must therefore upgrade host deployments before coordinator deployments.

Pre-upgrade host-owned child requests have no `requester_did` and cannot be
backfilled because the route key is immutable. After the new host-leg filter is
installed, those rows have no request-state convergence channel and terminal
re-drive correctly skips them. Drain in-flight cross-deployment children before
rolling out the new scope templates; then upgrade hosts first and coordinators
second.

## Write shape

At the pinned DefraDB revision, one GraphQL document update already produces
one composite root linking the modified field blocks and emits one update event
for the document. The incident's 78 blocks are 17 composite roots plus their
linked CRDT field blocks, not 78 independent document commits. #683 therefore
adds a regression fence around the composite-root count instead of replacing
the already-composite lifecycle mutations.

The excessive heights came from repeated logical transitions and terminal
re-drive. Party scoping and suppression of re-drive for unrouted requests
remove the multiplicative fleet fan-out at the sender. DefraDB #1102 remains
responsible for latest-head coalescing, encode-once fan-out, retry backoff, and
the transport coalescing window.

## Measurement

The acceptance measurement reuses the DefraDB #1103 push-backlog
`enqueued_total` signal with its workload shape: 13 request documents, 16 host
pairings, and rapid terminal/re-drive updates. A real two-node P2P test installs
the shipping `requester_did` filter, waits for the backlog to drain, re-drives
13 routed terminal documents, and observes exactly 13 outbound enqueues. The
former agent-scoped 16-pairing predicate produced 208 (= 13 × 16), so the
measured reduction is 16x. A second structural assertion pins the topology
calculation independently of transport timing:

- coordinator parent requests match zero host request replicators;
- each routed host child request matches exactly its one requester pairing;
- one re-drive pass over `N` routed child requests produces `N` request pushes,
  while local-only requests produce zero.

Against the prior `N × 16` request wave this is a 16x reduction, independently
of the additional transport reductions from DefraDB #1102.

## Sibling boundaries

- DefraDB #1101 fixes selective CAR lookup so requested child blocks can be
  served and acknowledged.
- DefraDB #1102 coalesces and deduplicates outbound transport work.
- DefraDB #1103 supplies the push-storm harness and symbolized profile.
- defra-agent #683 stops generating request-state work for unrelated peers.
