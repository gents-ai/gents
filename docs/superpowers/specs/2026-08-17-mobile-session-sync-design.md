# Mobile session sync — design

**Milestone:** [Mobile session sync](https://github.com/source-inc/gents/milestones/9) — #1141 → #1142 → #1143 → #1144, with #1145/#1146 independent.
**Status:** approved in session 2026-08-17; this document is the durable record.

## Problem

A freshly paired client (iOS or desktop) renders behaviors but no conversation
history, and sync activity is invisible. Root causes, established empirically
against a live pairing (amy on studio-1 ↔ iPhone):

1. The only history path, `sync_branchable_collections_with_retry`
   (`crates/gents-desktop-core/src/client/core/bootstrap.rs`), is gated behind
   `GENTS_DESKTOP_SYNC_BRANCHABLE_ON_PAIR`, default off and unsettable on iOS.
   Gossip subscriptions deliver new heads only, so collections that churn
   (behaviors, touched every reconcile) arrive while static history never does.
   A wiped phone held 37 documents against the server's 500+.
2. No sync state reaches the UI. The signals exist (`P2PHealth`,
   `PairingCollectionStatus`, observe-loop metrics, DefraDB `SyncStatus`) but
   are unexposed or FFI-unreachable.

## Constraints established by research

Four survey passes (Go DefraDB, defradb.rs at the pinned rev, the wider
sourcenetwork org, and a gents abstraction audit) fixed these load-bearing
facts:

- **Replication filter fields must be `@immutable` scalar LWW.** `session_id`
  is not `@immutable` on any transcript collection, so per-session replicator
  filters are rejected at install. `requester_did`/`agent_did` are `@immutable`
  — which is why the existing templates filter on them.
- **Any replicator filter change forces a full replay of every collection in
  the install** (defradb.rs `iroh.rs`), and the gents reconcile diff answers a
  filter change with teardown + reinstall. Filter churn is therefore not a
  hydration mechanism (filed upstream as defradb.rs#1506).
- **The ecosystem contract for query-shaped partial sync is app-side docID
  resolution + pull.** Go DefraDB punts predicate sync to the application
  (`SyncDocuments(col, docIDs)`); its only remote query primitive returns
  docIDs only. defradb.rs `sync_documents` fetches never-seen docs by ID
  (≤1000/request, no retry ladder, completion observed via `MergeComplete`).
- **gents already has the correct lifecycle shape to copy**: the
  `PersonaConfigRequest` pattern — a client-authored control document riding
  the `machine` template, admitted and served by a reconcile sweep
  (`p2p_reconcile/persona_requests.rs`), fenced by a Lean lifecycle model
  (`PersonaRequest.lean`: `admits`/`applyStep`, grants-nothing, idempotence,
  ownership safety).
- **In-flight upstream work to anticipate** (`fix/1116-sync-ownership-stage3`):
  selective-CAR frontier fetch, CAR served/filtered counters,
  reconnect-as-enabling-event. Do not build CID-level fetch or an app-side
  reconnect kick; read `SyncStatus` fields defensively.

## Design

Three mechanisms, split by job:

### 1. Standing scope and eager index request (#1141)

A new `client-index` scope template formalizes the requester-scoped index
contract: `AgentConversation` + `AgentSession`, `PerCollection` rules on
`@immutable` identity fields, `delivery = Push`. Full desktop pairings continue
to select `machine`; #1141 registers `client-index` for narrower consumers but
does not select it for the desktop pairing row.

The desktop P2P supervisor replaces the env-gated 16-collection bulk pull with
an ungated, concurrent BranchableSync request for exactly those two index
collections. It requests once after startup, for newly added peers, and again
after reconnect/repair, off the launch and add-peer critical paths. The pinned
IROH adapter acknowledges dispatch, not merge completion, so logs say
"requested" and the supervisor supplies new request opportunities at the
events it can observe. Exact progress/completion remains #1144/upstream work.

**Trust boundary:** BranchableSync authorizes at collection granularity and
returns every head in the collection; it does not apply the template's
document predicate. A paired full client therefore receives session cards for
all requester lineages on that agent. This is intentional for the current
single-user/full-client product, but it is not a tenant-safe use of the
`client-index` predicate and must not be reused for an untrusted peer class.

`write_peer_pairing_desired` explicitly selects `machine` instead of the old
implicit `conversation` fallback and drops the dead `collections` list. On the
first upgrade reconcile, existing template-absent pairings incur one teardown,
reinstall, and replay because their effective filter changes. This is an
accepted migration cost, not an index-hydration mechanism. The removed
`GENTS_DESKTOP_SYNC_BRANCHABLE_ON_PAIR` escape hatch has no 16-collection
replacement; transcript history remains lazy until #1142.

The session list (`AgentConversation` carries title, preview_text, updated_at,
fork lineage; `AgentSession` carries live status) renders with no eager
transcript-plane pull: about two small documents per session.

**Preferred end state:** replace the collection-wide index pull with a
paginated, cursor-based index protocol. The server should apply the explicitly
chosen lineage policy, return bounded pages of index document IDs, and let the
client fetch those pages with observable progress and resumable cursors. That
removes the current collection-level trust exception and scales without a
single all-head response; it is deliberately outside #1141.

### 2. Per-session hydration — `SessionHydrationRequest` (#1142, #1143)

A branchable control collection, mirroring `PersonaConfigRequest`:

```
SessionHydrationRequest {
  request_key   String @index(unique) @immutable   # "{peer_id}:{session_id}"
  requester_did String @index @immutable
  agent_did     String @immutable
  session_id    String @immutable
  created_at    DateTime
  status        String @index    # pending | served | rejected (server-written)
  status_detail String
  served_doc_count Int           # the client's progress denominator
  processed_at  DateTime
}
```

Client writes it on session open (upsert by `request_key`); it rides the
`machine` template to the server. A server sweep
(`p2p_reconcile/session_hydration.rs`, sibling of `persona_requests.rs`)
admits rows from paired, membership-valid peers whose session ownership checks
out, enumerates the session's docs across the transcript collections
(AgentRequest, AgentResponse, AgentMessage, AgentToolCall, AgentToolResult,
AgentToolApproval, CompactionEntry), pushes them through the **existing
doc-pusher under the existing admission bounds and persisted retry ladder** —
no new delivery machinery — then writes `served` + `served_doc_count`.
Failures write `rejected` + detail; retry is the client's decision.

**Lean first** (`proofs/Proofs/SessionHydration/`), mirroring
`PersonaRequest.lean`, with obligations:

1. `hydration_request_grants_nothing` — unadmitted rows cause no push.
2. **Tenancy soundness** — every served doc satisfies
   `requester_did = request.requester_did`. The replicator filter enforced
   this for standing scopes; a doc-set push must re-prove it at the admission
   gate. This is the load-bearing new obligation.
3. Session ownership — `session_id` resolves to a session owned by the
   requester (mirrors `unknown_agent_changes_nothing`).
4. Idempotent re-serve; crash repair.
5. Non-interference — `PairingReconcile.no_flap_on_converged` holds while
   hydration runs (formally justifies "separate lifecycle, not a scope layer").
6. Terminality — every pending row reaches served/rejected.

Plus CoverageLedger entry, `Executable.lean` contract, Rust conformance file,
proofs README update. Zero `sorry`s.

Client half (#1143): `ClientCore::request_session_hydration` (the first
client-authored control-request write path — establishes the pattern),
`focused_session_id` on `ObservedStore`, a hydration/progress `watch` channel
beside `p2p_health`, and `P2PSupervisorCommand::{Foregrounded,
NetworkChanged}` reusing the existing repair cycle. Progress is
receiver-side: `served_doc_count` is the denominator, locally merged docIDs
the numerator. Hydrated sessions persist on-device; no eviction in v1.

### 3. Indicators — surface what exists (#1144)

- `PairingCollectionStatus` (retry counts, error class, `stuck_since`) is
  computed every tick and never exposed — the cheapest win.
- Bridge: `desktop_session_hydrate`, `desktop_sync_status`,
  `SessionHydrationView`, `PairingCollectionStatusView`,
  `ClientUpdateEvent.reason += "hydration"`.
- UI: global sync pill (healthy / syncing / stalled / offline-since),
  per-session hydration progress, settings debug panel with raw counters.
  Stalled (retry class + stuck_since) renders distinctly from syncing;
  quarantined DAGs are terminal errors, not spinners.

### Independent

- **#1145** — widen `PairingFilters` to the transport's predicate model;
  `merge_layered_desired` currently last-writer-wins overlapping collection
  filters silently (the #687 leak class).
- **#1146** — mobile Behaviors page has no back navigation.

## Rejected alternatives

- **Per-session filtered replicators** — `session_id` not `@immutable`;
  filter churn forces full replay + reinstall on links that idle-close.
- **Scope-template parameterization** — `&'static` catalog, DID-only value
  space, one predicate per collection; a session layer would silently
  overwrite the tenancy fence in `merge_layered_desired`.
- **Full branchable bulk sync** (flip the env gate) — pulls every transcript
  ever to every device; the mobile storage/bandwidth profile is wrong.
- **Server answers with docID list, client pulls `sync_documents`** — viable
  (the Go-blessed shape) but the server push path reuses the persisted retry
  ladder for free, keeps a single delivery mechanism, and `sync_documents`
  broadcasts to all connected peers rather than targeting the server.

## Upstream issues filed

- defradb.rs#1504 — `SyncStatus` unreachable from FFI/embedded clients.
- defradb.rs#1505 — `ReplicatorCompleted` has no payload.
- defradb.rs#1506 — iroh/libp2p replay-trigger asymmetry; filter change
  replays all collections.

None block the critical path.

## Testing

Lean models + conformance per the foundation flow; Rust unit tests for
enumeration/tenancy filtering; a two-node e2e (server-authored session,
client pairs, index syncs, hydration round-trips, progress observed) in the
existing e2e harness. Gates: `cargo test -p gents`,
`cargo check --workspace --all-targets`.
