# Mobile session sync — design

> Authority update (2026-08-31): authenticated enrollment, signed route
> receipts, and current authorization now own this transport. Older
> pairing/membership language below records the design as originally shipped;
> it is not a compatibility path.

**Milestone:** [Mobile session sync](https://github.com/source-inc/gents/milestones/9) — #1141 → #1142 → #1143 → #1144, with #1145/#1146 independent.
**Status:** shipped on current `main` plus the `mobile-session-sync/00`–`04` productization stack (2026-08-27). The original 2026-08-17 session approval still holds; the reductions below are the durable record of what actually landed.

## What shipped

Index cards (`AgentConversation` + `AgentSession`) sync eagerly. Transcript history stays lazy: a client writes `SessionHydrationRequest` on session focus, the runtime serves the exact tenant-scoped document set through the existing peer-targeted doc pusher, and the receiver declares completion only after locally merged documents cover `served_doc_count`.

Productization on top of merged PR #1154:

- A pure desktop-core projection derives `healthy | syncing | stalled | offline | failed` from `P2PHealth`, pairing/route retry, and `ClientHydrationProgress`. Terminal/quarantined failure and stalled never collapse into syncing; offline never pretends progress is active.
- The bridge projects pairing collection status, hydration, and that derived summary on existing snapshots. `desktop://client-updated` gains `reason = "hydration"`. There is no second request lifecycle, polling loop, or `desktop_sync_status` command. Passive snapshots only start an idle target; failed targets remain terminal until `desktop_session_hydration_retry` explicitly restarts them.
- Selected-session UI renders already-local transcript immediately and shows requested / serving N-of-M / complete / failed, with retry on failed. Hydration wakes refresh the selected session and the runtime snapshot (global `syncHealth` lives on the snapshot). Unchanged receiver progress does not re-emit. Delayed snapshots cannot replace another session.
- A self-contained global indicator on the current fleet and chat surfaces distinguishes the five states and opens pairing/route/error-class/stuck-since/hydration diagnostics. It does not depend on PR #1153 navigation work.
- CI acceptance uses the existing desktop UI harness (`session-hydration`, `sync-offline`, `sync-stalled`, `sync-failed`). The ignored live two-node test remains the protocol/runtime qualification:

```sh
GENTS_LIVE_SESSION_HYDRATION=1 cargo test -p gents --features live-e2e \
  --test e2e_live live_session_hydration_replays_history_to_a_fresh_client \
  -- --ignored --nocapture
```

### Reductions from the original issue text

- No `P2PSupervisorCommand::{Foregrounded, NetworkChanged}`. Suspend/resume recovery stays on the existing route repair / `desktop_p2p_repair` path.
- No dedicated hydration-start or sync-status Tauri commands. Initial focus stays snapshot-owned; terminal retry is an explicit session-control command.
- Per-session replicator filters remain rejected (`session_id` is not `@immutable`).
- Upstream defradb.rs#1504 FFI `p2p_sync_status` is still not required. Quarantined/permanent rejection is `PairingErrorClass::RemoteUnauthorized` plus hydration `failed`.

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
CompactionEntry), pushes them through the **existing
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

Client half (#1143): the hydration request writer is the first client-authored
control-request write path. The desktop bridge reaches it from an idle
`desktop_session_snapshot` or an explicit failed-state retry. Both reuse the
existing request observation and P2P repair paths rather than adding
focus/network commands or a second progress channel. Progress is
receiver-side: `served_doc_count` is the denominator, locally merged docIDs
the numerator. Hydrated sessions persist on-device; no eviction in v1.

### 3. Indicators — surface what exists (#1144)

- `PairingCollectionStatus` (retry counts, error class, `stuck_since`) is
  computed every tick and never exposed — the cheapest win.
- Bridge: `SessionHydrationView`, `PairingCollectionStatusView`,
  `SyncHealthView` on existing snapshots, and
  `ClientUpdateEvent.reason += "hydration"`. No extra hydrate/status commands.
- UI: global sync indicator (healthy / syncing / stalled / offline-since /
  failed) plus a diagnostics panel with raw counters, and per-session
  hydration progress. Stalled (retry class + stuck_since) renders distinctly
  from syncing; quarantined/unauthorized work is a terminal error, not a spinner.

Superseded by bridge contract 2.0: global sync health now owns only transport
and configured-peer pairing/routes. Hydration is derived from the exact durable
control row in each session snapshot, and `SessionHydrationRequest` changes wake
that projection through the ordinary `store` event. There is no global
hydration field or dedicated hydration event reason.

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
