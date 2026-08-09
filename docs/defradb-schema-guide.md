# DefraDB schema guide

Gents uses DefraDB as its control plane and durable fact store. Runtime code
should write the facts consumers need and let DefraDB provide document identity,
content addressing, signing, replication, and later ACP enforcement. Do not add
a second application-level control plane for properties the database already
supplies.

## Ingest and presentation

Ingest is responsible for durably recording every fact needed to explain an
agent run. Presentation is a replaceable projection over those facts. A CLI,
desktop view, audit export, or adapter may reshape data, but it must not depend
on process-local state or guess which duplicate or mutable document was used.

Before adding a collection or field, identify a real projection that consumes
it. Before adding a new runtime abstraction, determine whether a DefraDB
document, version, signature, query, or replication feature already expresses
the requirement.

## Document identity and provenance

These identifiers have different jobs:

- A logical id such as `request_id` is a correlation key for people and APIs.
  It is not sufficient evidence when multiple documents can carry it.
- `_docID` identifies one DefraDB document. Durable relationships should carry
  it when they must name a physical source fact.
- A composite commit CID identifies one exact version of that document and is
  the DefraDB time-travel reference.
- Commit signature evidence identifies the database principal that authored a
  version. A DID stored in an ordinary field is only application data.

Do not add a hash of content merely to restate DefraDB's content addressing.
Keep hashes only when they are query indexes or injective idempotency keys, and
document that purpose. Never describe an application-supplied digest as proof
of stored content.

Queries by logical id must either prove the expected cardinality or fail closed
when more than one physical document matches. `order` plus `limit: 1` is not a
uniqueness invariant.

## Identity on database operations

Every production embedded node that writes documents must have a registered
signing identity and a configured node DID. Normal reads and writes go through
the shared identity-aware execution path so future policies see an actor and
writes receive DefraDB commit signatures.

`requester_did`, GraphQL/ACP query identity, and commit signer are distinct:

- `requester_did` records participant lineage.
- query identity is the actor evaluated by policy.
- the commit signer is durable authorship evidence.

A node-authored request can retain the initiating participant in
`requester_did`. Proving a remote HTTP caller, rather than the storage node,
requires a signed request envelope; a self-reported DID is not enough.

ACP policies and encryption are separate later workstreams. Schema and writer
design should leave room for them without claiming that they are already
enforced.

## Branchability and replication

`@branchable` is not a history switch and does not create provenance. DefraDB
already content-addresses document versions. Branchability enables the
collection-level DAG used for peer catch-up and is a prerequisite for
collection-scoped distributed behavior.

Use `@branchable` by default for shared canonical facts and desired
control-plane configuration that must support late-peer backfill or future
collection-scoped ACP. Keep deployment-local leases, cursors, health state,
replaceable caches, local filesystem configuration, and secrets non-branchable
unless a concrete multi-host contract requires otherwise.

Live update delivery is a wake-up hint. Correctness comes from durable queries
and reconciliation, so a restart, missed notification, or late peer can recover
from the database.

Branchability and collection policy are immutable root properties in the
pinned DefraDB. During the current pre-release redesign we treat changes to
those roots as intentionally breaking. Later compatible evolution should use
Lens and an explicit successor/backfill plan.

## Review checklist

For every collection, record:

- the represented fact or entity and its source of truth;
- logical-id scope, `_docID` relationships, and exact-CID requirements;
- creator, transition writers, and expected commit signer;
- immutable facts versus mutable lifecycle or observed state;
- branchability, replication filter, and host-local exceptions;
- projection consumers and the fields they actually require;
- hot queries, cardinality assumptions, and justified indexes;
- retention/archive class and compatible-evolution plan;
- whether a lifecycle invariant requires Lean and conformance changes.

The preferred implementation shape is small: improve the shared DefraDB
execution/query helpers, persist a missing source edge on an existing fact, and
make consumers use it. Add a new collection or runtime primitive only when the
existing fact graph cannot represent the requirement.
